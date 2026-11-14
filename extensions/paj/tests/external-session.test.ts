import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { connect } from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { BridgeServer } from '../bridge.ts';
import { createExternalSession } from '../external-session.ts';

const identity = { id: 'paj', pid: 123, piSessionId: 'pi', role: 'primary' };

test('versioned registration readiness, defensive snapshots and removal', () => {
  let changes = 0;
  const session = createExternalSession(() => false, () => changes++);
  assert.equal(session.api.version, 1);
  assert.equal(session.api.getSnapshot().identity, null);
  assert.equal(session.api.tryReserve('remote'), false);
  session.update(identity);
  assert.deepEqual(session.api.getSnapshot().identity, identity);
  session.api.getSnapshot().identity!.id = 'changed';
  assert.equal(session.api.getSnapshot().identity!.id, 'paj');
  assert.equal(session.api.tryReserve('remote'), true);
  session.update(null);
  assert.equal(session.isReserved(), false);
  assert.equal(session.api.getSnapshot().identity, null);
  assert.equal(changes, 3);
});

test('editor and external admission exclude each other without editor request state', () => {
  let editor = true;
  const session = createExternalSession(() => editor, () => {});
  session.update(identity);
  assert.equal(session.api.tryReserve('browser'), false);
  editor = false;
  assert.equal(session.api.tryReserve('browser'), true);
  assert.equal(session.api.tryReserve('browser'), true);
  assert.equal(session.api.tryReserve('another-browser'), false);
  assert.equal(session.isReserved(), true);
  assert.equal(session.api.getSnapshot().editorBusy, false);
  session.api.release('unrelated');
  assert.equal(session.isReserved(), true);
  session.api.release('browser');
  assert.equal(session.isReserved(), false);
  session.api.tryReserve('second');
  session.clearReservations();
  assert.equal(session.isReserved(), false);
});

test('real bridge admission honors external reservations without creating editor policy', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'paj-external-'));
  const socketPath = join(directory, 'bridge.sock');
  let sends = 0;
  let bridge: BridgeServer;
  const external = createExternalSession(() => Boolean(bridge?.getActiveRequest()), () => {});
  bridge = new BridgeServer({
    isIdle: () => !external.isReserved(),
    sendRequest() { sends++; },
    cancelRequest() {},
  });
  const request = async () => {
    const socket = connect(socketPath);
    const frames: Array<Record<string, any>> = [];
    let buffer = '';
    const firstFrame = new Promise<Record<string, any>>((resolve, reject) => {
      socket.on('error', reject);
      socket.on('data', (chunk) => {
        buffer += chunk;
        let end: number;
        while ((end = buffer.indexOf('\n')) !== -1) {
          const frame = JSON.parse(buffer.slice(0, end));
          buffer = buffer.slice(end + 1);
          frames.push(frame);
          resolve(frame);
        }
      });
    });
    socket.write(JSON.stringify({ id: '019fa92e-a7c2-7072-84a7-8933262464a5', method: 'request', params: {
      kind: 'query', query: 'Hello', source: { path: '/tmp/example', startLine: 1, endLine: 1, content: 'x' },
    } }) + '\n');
    return { socket, first: await firstFrame, frames };
  };
  try {
    await bridge.start(socketPath);
    external.update(identity);
    external.api.tryReserve('browser');
    const rejected = await request();
    assert.equal(rejected.first.code, 'busy');
    assert.equal(bridge.getActiveRequest(), undefined);
    assert.equal(sends, 0);
    rejected.socket.destroy();
    external.api.release('browser');
    const accepted = await request();
    assert.equal(accepted.first.event, 'accepted');
    assert.equal(external.api.getSnapshot().editorBusy, true);
    assert.equal(external.api.tryReserve('browser'), false);
    assert.equal(sends, 1);
    accepted.socket.destroy();
  } finally {
    await bridge.stop();
    await rm(directory, { recursive: true, force: true });
  }
});

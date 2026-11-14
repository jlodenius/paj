export interface ExternalIdentity {
  id: string;
  pid: number;
  piSessionId?: string;
  role: string;
  name?: string;
  projectRoot?: string;
  branch?: string;
  parentPiSessionId?: string;
}

export function createExternalSession(
  editorBusy: () => boolean,
  changed: () => void,
) {
  let identity: ExternalIdentity | null = null;
  const reservations = new Set<string>();
  const api = {
    version: 1,
    getSnapshot: () => ({
      identity: identity ? { ...identity } : null,
      editorBusy: editorBusy(),
      externalBusy: reservations.size > 0,
    }),
    tryReserve(token: string) {
      if (!identity || editorBusy() || !token) return false;
      if (reservations.has(token)) return true;
      if (reservations.size) return false;
      reservations.add(token);
      changed();
      return true;
    },
    release(token: string) {
      if (reservations.delete(token)) changed();
    },
  };
  return {
    api,
    isReserved: () => reservations.size > 0,
    update(value: ExternalIdentity | null) {
      identity = value ? { ...value } : null;
      if (!value) reservations.clear();
      changed();
    },
    clearReservations() {
      if (!reservations.size) return;
      reservations.clear();
      changed();
    },
    notify: changed,
  };
}

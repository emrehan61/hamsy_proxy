// IndexedDB persistence for imported HAR sessions, so an open session
// survives a reload and can be opened by id from a separate browser window
// (e.g. a HAR "pop out" tab). Persistence is a nice-to-have, not a
// requirement: every exported function degrades to a no-op (resolving to
// `undefined`/`[]`/void plus a `console.warn`) whenever IndexedDB is
// unavailable or throws — private browsing, quota exceeded, disabled
// storage, etc. Callers must never `.catch()` these; they don't reject.

import type { Flow } from "./types";
import type { HarPageInfo } from "./har";

export interface StoredHarSession {
  id: string;
  name: string;
  importedAt: number;
  flowCount: number;
  flows: Flow[];
  pages: HarPageInfo[];
  creatorName: string;
  creatorVersion: string;
}

const DB_NAME = "flproxy-har";
const DB_VERSION = 1;
const STORE_NAME = "sessions";
const IMPORTED_AT_INDEX = "importedAt";

// Cached across calls: IndexedDB.open is relatively expensive and every
// exported function needs a handle, so open it once and reuse it (or reuse
// the same "unavailable" verdict, `null`, if opening ever failed).
let dbPromise: Promise<IDBDatabase | null> | null = null;

function openDb(): Promise<IDBDatabase | null> {
  if (dbPromise) return dbPromise;
  dbPromise = new Promise((resolve) => {
    if (typeof indexedDB === "undefined") {
      resolve(null);
      return;
    }
    try {
      const req = indexedDB.open(DB_NAME, DB_VERSION);
      req.onupgradeneeded = () => {
        const db = req.result;
        if (!db.objectStoreNames.contains(STORE_NAME)) {
          const store = db.createObjectStore(STORE_NAME, { keyPath: "id" });
          store.createIndex(IMPORTED_AT_INDEX, IMPORTED_AT_INDEX, { unique: false });
        }
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => {
        console.warn("flproxy: failed to open HAR IndexedDB", req.error);
        resolve(null);
      };
    } catch (err) {
      console.warn("flproxy: IndexedDB unavailable", err);
      resolve(null);
    }
  });
  return dbPromise;
}

function runTransaction(db: IDBDatabase, mode: IDBTransactionMode, run: (store: IDBObjectStore) => void): Promise<void> {
  return new Promise((resolve, reject) => {
    const tx = db.transaction(STORE_NAME, mode);
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
    tx.onabort = () => reject(tx.error);
    run(tx.objectStore(STORE_NAME));
  });
}

export async function putSession(s: StoredHarSession): Promise<void> {
  const db = await openDb();
  if (!db) return;
  try {
    await runTransaction(db, "readwrite", (store) => {
      store.put(s);
    });
  } catch (err) {
    console.warn(`flproxy: failed to persist HAR session ${s.id}`, err);
  }
}

export async function getSession(id: string): Promise<StoredHarSession | undefined> {
  const db = await openDb();
  if (!db) return undefined;
  try {
    return await new Promise<StoredHarSession | undefined>((resolve, reject) => {
      const tx = db.transaction(STORE_NAME, "readonly");
      const req = tx.objectStore(STORE_NAME).get(id);
      req.onsuccess = () => resolve((req.result as StoredHarSession | undefined) ?? undefined);
      req.onerror = () => reject(req.error);
    });
  } catch (err) {
    console.warn(`flproxy: failed to load HAR session ${id}`, err);
    return undefined;
  }
}

/** Cursors over the store and strips `flows`/`pages` per row — HAR files can be tens of MB, so listing sessions must never pull full flow arrays into memory. */
export async function listSessionMetas(): Promise<Omit<StoredHarSession, "flows" | "pages">[]> {
  const db = await openDb();
  if (!db) return [];
  try {
    return await new Promise((resolve, reject) => {
      const metas: Omit<StoredHarSession, "flows" | "pages">[] = [];
      const tx = db.transaction(STORE_NAME, "readonly");
      const store = tx.objectStore(STORE_NAME);
      const source = store.indexNames.contains(IMPORTED_AT_INDEX) ? store.index(IMPORTED_AT_INDEX) : store;
      const req = source.openCursor();
      req.onsuccess = () => {
        const cursor = req.result;
        if (!cursor) {
          resolve(metas);
          return;
        }
        const value = cursor.value as StoredHarSession;
        metas.push({
          id: value.id,
          name: value.name,
          importedAt: value.importedAt,
          flowCount: value.flowCount,
          creatorName: value.creatorName,
          creatorVersion: value.creatorVersion,
        });
        cursor.continue();
      };
      req.onerror = () => reject(req.error);
    });
  } catch (err) {
    console.warn("flproxy: failed to list HAR sessions", err);
    return [];
  }
}

export async function deleteSession(id: string): Promise<void> {
  const db = await openDb();
  if (!db) return;
  try {
    await runTransaction(db, "readwrite", (store) => {
      store.delete(id);
    });
  } catch (err) {
    console.warn(`flproxy: failed to delete HAR session ${id}`, err);
  }
}

export async function clearAllSessions(): Promise<void> {
  const db = await openDb();
  if (!db) return;
  try {
    await runTransaction(db, "readwrite", (store) => {
      store.clear();
    });
  } catch (err) {
    console.warn("flproxy: failed to clear HAR sessions", err);
  }
}

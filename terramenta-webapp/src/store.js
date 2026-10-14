/**
 * Key-value persistence in IndexedDB (`terramenta` / `kv`). Every call is best
 * effort: with storage unavailable (private mode, blocked site data) reads
 * resolve `undefined` and writes are dropped with a warning.
 */

const DB_NAME = "terramenta";
const STORE_NAME = "kv";

let opening = null;

function database() {
  opening ??= new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, 1);
    request.onupgradeneeded = () => request.result.createObjectStore(STORE_NAME);
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
  return opening;
}

async function transact(mode, operate) {
  const db = await database();
  return new Promise((resolve, reject) => {
    const transaction = db.transaction(STORE_NAME, mode);
    const request = operate(transaction.objectStore(STORE_NAME));
    transaction.oncomplete = () => resolve(request.result);
    transaction.onerror = () => reject(transaction.error);
    transaction.onabort = () => reject(transaction.error);
  });
}

export function get(key) {
  return transact("readonly", (store) => store.get(key)).catch((error) => {
    console.warn(`Store: cannot read ${key}`, error);
    return undefined;
  });
}

export function set(key, value) {
  return transact("readwrite", (store) => store.put(value, key)).catch((error) =>
    console.warn(`Store: cannot write ${key}`, error),
  );
}

export function remove(key) {
  return transact("readwrite", (store) => store.delete(key)).catch((error) =>
    console.warn(`Store: cannot delete ${key}`, error),
  );
}

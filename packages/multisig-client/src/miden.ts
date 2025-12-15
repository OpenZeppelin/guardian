/**
 * Miden SDK utilities.
 */

// =============================================================================
// IndexedDB Utilities
// =============================================================================

/**
 * Clear all IndexedDB databases.
 * Useful for resetting the Miden client state in browser environments.
 */
export async function clearIndexedDB(): Promise<void> {
  if (typeof indexedDB === 'undefined') {
    return; // Not in browser environment
  }

  const databases = await indexedDB.databases();
  const deletePromises = databases
    .filter((db) => db.name)
    .map(
      (db) =>
        new Promise<void>((resolve, reject) => {
          const request = indexedDB.deleteDatabase(db.name!);
          request.onsuccess = () => resolve();
          request.onerror = () => reject(request.error);
          request.onblocked = () => resolve();
        })
    );
  await Promise.all(deletePromises);
}

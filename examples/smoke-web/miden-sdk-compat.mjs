// Compat shim for @miden-sdk/miden-sdk 0.15.
//
// The Para wallet-adapter tree (@miden-sdk/react@0.14 → use-miden-para-react)
// is still built against the 0.14 SDK and imports the `NoteAttachmentKind`
// enum, which 0.15 removed in favour of the `NoteAttachment`/`NoteAttachmentScheme`
// class refactor. Until the Para packages ship a 0.15-compatible release this
// shim re-exports the real 0.15 SDK and restores `NoteAttachmentKind` with its
// original 0.14 values so the adapter modules link and load.
export * from './node_modules/@miden-sdk/miden-sdk/dist/st/eager.js';

export const NoteAttachmentKind = Object.freeze({
  None: 0,
  '0': 'None',
  Word: 1,
  '1': 'Word',
  Array: 2,
  '2': 'Array',
});

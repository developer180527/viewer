# Editing UX

Status: proposal. Nothing here is built yet.

**The rule: learn it once.** Someone who has edited one kind of file already knows where everything is for every other kind. Only the tools differ, never where they live or how a session starts and ends.

## 1. The layout, the same on every file

```
Viewing                                  Editing
┌────────────────────────────────┐       ┌────────────────────────────────┐
│ Open…  name.pdf     [Edit] Info│       │ Done  ↶ ↷   name.pdf •    Save │  bar: session
├────────────────────────────────┤       ├────────────────────────────────┤
│                                │       │                                │
│             content            │       │     content, still zoomable    │
│                                │       │                                │
│                                │       │       ┌ options of the tool ┐  │  appears only
│                                │       │       └─────────────────────┘  │  when needed
│                                │       │  ┌ Select│Add│Mark up│Arrange┐ │  bottom: tools
└────────────────────────────────┘       └────────────────────────────────┘
```

- **Edit** is always in the same spot in the top bar, on every file and platform. On phones it stays visible; Info moves to the `…` menu first.
- **The top bar holds the session:** Done, Undo, Redo, the name with a dot when there are unsaved changes, Save.
- **The bottom strip holds the tools,** on desktop too, so phone and desktop teach the same thing and thumbs reach it.
- **Tool options** (colour, thickness, font size) float just above the strip, and only for the tool in use.
- **The content view doesn't change:** scrolling, zooming and panning work as in viewing. On touch, one finger uses the tool and two fingers scroll and zoom. With `Select` active, one finger scrolls as usual.

## 2. Four tool groups, always in this order

| Group | Means | PDF | Image | Text, code, Markdown | Table, JSON, TOML | Archive, folder | 3D | Audio, video |
|---|---|---|---|---|---|---|---|---|
| **Select** | Pick and move what's there; the default | Annotations, form fields | Layers, marks | Cursor and selection | Cell, node | Entries | Object | Playhead |
| **Add** | Put something new in | Text box, image, signature image, fill form | Text, image | Typing, paste | Row, column, key | Add files, new folder | Import, primitive | Insert clip |
| **Mark up** | Draw attention, on top of the content | Highlight, underline, pen, shape, note | Pen, shape, arrow | Comment (Markdown) | Cell colour (where stored) | Not shown | Measure, note | Marker |
| **Arrange** | Change the shape or order | Pages: rotate, reorder, delete, merge | Crop, rotate, resize, adjust | Sort lines, find and replace | Sort, reorder, resize | Rename, move, delete | Move, rotate, scale, delete | Trim, split, reorder |

An empty group is hidden, and the others keep their order. Delete is never a separate group: it acts on the selection, through the `Delete` key or the selection's menu.

## 3. Starting, saving and leaving

Same on every file:

- **Nothing touches the original until Save.** Save writes a temporary file and swaps it in, so a crash can't leave half a file.
- **Save a copy** sits next to Save in its menu. It replaces Save when the file can't be written: inside an archive, read-only, or iOS without write access.
- **Warnings come before Save, only when something real is lost:**
  - Saving breaks a digital signature.
  - The format loses quality or features (JPEG recompressed, FBX features dropped). The option is then to save a copy in a better format.
- **Done with unsaved changes** asks: Save, Discard, Keep editing.
- **Undo** covers the whole session and survives Save until Done.
- **A crash loses little:** edits are journalled next to the app's data, and the next open offers to restore them.
- **If the file changed on disk meanwhile,** Save asks: overwrite or save a copy.

## 4. Shortcuts, the same everywhere

| Key | Does |
|---|---|
| `E` | Enter edit mode |
| `Esc` | Back to Select; pressed again, Done |
| `Cmd/Ctrl-Z`, `Shift-Cmd/Ctrl-Z` | Undo, redo |
| `Cmd/Ctrl-S`, `Shift-Cmd/Ctrl-S` | Save, save a copy |
| `V`, `A`, `M`, `R` | Select, Add, Mark up, Arrange (not while typing in a file) |
| `Delete` | Delete the selection |

## 5. When editing isn't available

The Edit button never disappears, so nobody is left wondering whether they've missed it. On a file that can't be edited here, it opens a short explanation:

- **Not built yet:** "Editing FBX files isn't available yet."
- **Not on this platform:** "Redaction isn't available on iPhone and iPad. It's available on macOS, Windows, Linux and Android."
- **Locked or encrypted:** says what would unlock it.

Tools missing on one platform aren't shown greyed out. Where someone would look for one, the group has a one-line note (docs/pdf.md §2).

## 6. Structure

One shell in the app, plus a small part per format:

```rust
// viewer-core: no UI.
pub trait EditSession: Send {
    fn tools(&self) -> Vec<ToolGroup>;      // what appears in the four groups
    fn apply(&mut self, op: Box<dyn EditOp>) -> Result<()>;
    fn undo(&mut self) -> bool;             // the shell owns when, the session owns how
    fn redo(&mut self) -> bool;
    fn can_save(&self) -> SaveTarget;       // in place, copy only, with which warnings
    fn save(&mut self, to: &mut dyn Write) -> Result<()>;
}
```

- A loader reports whether it can edit a document, and why not if it can't; `Document` carries that.
- **The shell** (bars, undo, save, journal, leave prompts, shortcuts) is written once and never knows the format.
- **The format** supplies its tools, its operations, and how they draw over the content view.
- **The PDF backends** stay behind a trait: PDFKit and MuPDF each implement the same operations. An operation only one of them has is simply absent from `tools()` on the other.

## 7. Build order

1. The shell, with **plain text** as its first format: the simplest test of Edit, undo, Save and leaving.
2. **PDF:** Mark up (highlight, pen, note) on both backends, then Arrange (pages), then forms.
3. **Images:** crop, rotate, mark up.
4. Tables and data, archives, then the others as their viewers mature.

# Viewer's documentation

How the docs are organised, and what to read first.

## Read in this order

| Doc | Answers |
|---|---|
| [../PLAN.md](../PLAN.md) | What Viewer is: goals, formats and libraries, platforms, startup, security, licensing, and the decided architecture (§12) |
| [roadmap.md](roadmap.md) | What comes next, in what order, and why |
| [decisions.md](decisions.md) | Every decision so far, dated, with where it's explained |
| [design/document-model.md](design/document-model.md) | **How a file is represented:** readings, documents and their facets. Everything below builds on it |
| [design/libfileparser.md](design/libfileparser.md) | The parsing libraries: what every category library must be (complete, a stable flat API, trivially extended, embeddable), the crates, hooks, front door, and how they are tested |
| [design/libfileparser-3d.md](design/libfileparser-3d.md) | The 3D parsing library, format by format |
| [design/architecture.md](design/architecture.md) | The two halves, libfileparser (reading) and libviewer (rendering), the products around them, the rules they follow, and the audit of third-party libraries |
| [design/libviewer.md](design/libviewer.md) | The rendering library: a view for each content model, what the app hosts, and what stays in the app |

Then the design of each part, as needed:

| Doc | Part | Status |
|---|---|---|
| [design/pdf.md](design/pdf.md) | PDF: MuPDF and PDFKit behind `PageSource`, the tiled page view | Built; plan in §10 |
| [design/3d-renderer.md](design/3d-renderer.md) | RendererX, the wgpu 3D renderer | Built; milestones in §16 |
| [design/responsive-ui.md](design/responsive-ui.md) | One layout from phone to desktop | Built in part |
| [design/editing.md](design/editing.md) | Editing: one session shell for every format | Proposal; waits on the roadmap |

Outside `docs/`: [../README.md](../README.md) (running and developing), [../corpus/README.md](../corpus/README.md) (the test corpus).

## Conventions

- **One topic per doc.** A design doc owns its topic; others link to it rather than repeat it. If two docs disagree, the one owning the topic wins, and the other is fixed.
- **Every design doc starts with a status line:** proposal, agreed (with the date), or built, and what of it exists.
- **Decisions are dated** where they're made, and listed in [decisions.md](decisions.md) with a link.
- **Status marks:** ✅ done · 🟡 partly done · ⬜ not started.
- **References** name the file from the repository root and the section: `docs/design/pdf.md §3`. Code comments use the same form, so a moved doc is found by one search.
- **Plain English,** short sentences, tables for comparisons.

import { memo } from "react";
import { markdownFormatEvent } from "@rightkit/document-core/app/shell/nativeMenu";
import { useSettings } from "@rightkit/document-core/settings/settings";
import type { LoadedDocument, ViewMode } from "@rightkit/document-core/documents/types";
import { capabilitiesFor } from "../documents/capabilities.js";
import type { DocxMenuApi } from "@rightkit/docx-editor/documents/docx/menuApi";
import { Dropdown, Item } from "@rightkit/document-core/documents/toolbarPrimitives";
import { shortcutLabel, isMac } from "@rightkit/document-core/platform";
import {
  type NativeMenuCommand,
  nativeMenuActive,
  useNativeDocumentMenu,
} from "@rightkit/document-core/app/shell/nativeMenu";
import { hostAppName } from "../appName.js";
import { documentEvent } from "@rightkit/document-core/host";

interface Props {
  doc: LoadedDocument;
  view: ViewMode;
  onSave: () => void;
  onSaveAs: () => void;
  onPrint: () => void;
  onClose: () => void;
  onChangeView: (v: ViewMode) => void;
  onEditImageExternal?: () => void;
  /** Present once a DOCX editor is mounted — drives docx's menus + save/print. */
  docxMenu?: DocxMenuApi | null;
  /** Wave 2 Read mode — menu/view-switch editing affordances hide; file + viewing actions stay. */
  readOnly?: boolean;
}

export const DocumentMenus = memo(DocumentMenusImpl);

/** Menu title that hides the Windows-style Alt-letter accelerator hint on macOS. */
function menuTitle(label: string, accelerator: string): string {
  return isMac() ? label : `${label} (Alt+${accelerator})`;
}

function DocumentMenusImpl({
  doc,
  view,
  onSave,
  onSaveAs,
  onPrint,
  onClose,
  onChangeView,
  onEditImageExternal,
  docxMenu,
  readOnly = false,
}: Pick<
  Props,
  | "doc"
  | "view"
  | "onSave"
  | "onSaveAs"
  | "onPrint"
  | "onClose"
  | "onChangeView"
  | "onEditImageExternal"
  | "docxMenu"
  | "readOnly"
>) {
  // macOS: these commands live in the system menu bar, not in the window.
  if (nativeMenuActive())
    return (
      <NativeDocumentMenus
        doc={doc}
        view={view}
        onSave={onSave}
        onSaveAs={onSaveAs}
        onPrint={onPrint}
        onClose={onClose}
        onChangeView={onChangeView}
        onEditImageExternal={onEditImageExternal}
        docxMenu={docxMenu}
        readOnly={readOnly}
      />
    );
  if (doc.format === "docx" && docxMenu)
    return <DocxMenus api={docxMenu} onSaveAs={onSaveAs} onClose={onClose} readOnly={readOnly} />;
  if (doc.format === "web") return null;
  return (
    <TextMenus
      doc={doc}
      view={view}
      onSave={onSave}
      onSaveAs={onSaveAs}
      onPrint={onPrint}
      onClose={onClose}
      onChangeView={onChangeView}
      onEditImageExternal={onEditImageExternal}
      readOnly={readOnly}
    />
  );
}

/** Commands the in-window menus offer for this document, for the macOS menu bar. Mirrors the
 *  visibility rules of DocxMenus/TextMenus below so both platforms expose the same actions. */
export function documentMenuCommands({
  doc,
  view,
  onChangeView,
  onEditImageExternal,
  docxMenu,
  readOnly = false,
  spellcheck,
  wordWrap,
  toggleSpellcheck,
  toggleWordWrap,
}: {
  doc: LoadedDocument;
  view: ViewMode;
  onChangeView: (v: ViewMode) => void;
  onEditImageExternal?: () => void;
  docxMenu?: DocxMenuApi | null;
  readOnly?: boolean;
  spellcheck: boolean;
  wordWrap: boolean;
  toggleSpellcheck: () => void;
  toggleWordWrap: () => void;
}): NativeMenuCommand[] {
  if (doc.format === "web") return [];
  if (doc.format === "docx" && docxMenu) {
    const api = docxMenu;
    const commands: NativeMenuCommand[] = [{ id: "page-setup", run: () => api.pageSetup() }];
    if (readOnly) return commands;
    const fmt = (id: string, action: Parameters<DocxMenuApi["format"]>[0]) => ({
      id,
      run: () => api.format(action),
    });
    return [
      ...commands,
      fmt("bold", "bold"),
      fmt("italic", "italic"),
      fmt("underline", "underline"),
      fmt("strikethrough", "strikethrough"),
      fmt("superscript", "superscript"),
      fmt("subscript", "subscript"),
      fmt("clear-formatting", "clearFormatting"),
      { id: "insert-table", run: () => api.insertTable(3, 3) },
      { id: "insert-image", run: () => api.insertImage() },
      fmt("insert-link", "insertLink"),
      { id: "insert-page-break", run: () => api.insertPageBreak() },
      { id: "insert-toc", run: () => api.insertTOC() },
    ];
  }
  const cap = capabilitiesFor(doc.format);
  const editable = cap.canReplace;
  const commands: NativeMenuCommand[] = [];
  if (doc.format === "image" && onEditImageExternal)
    commands.push({ id: "edit-image-external", run: onEditImageExternal });
  if (cap.canSwitchView && doc.format === "html") {
    commands.push({
      id: "view-interactive",
      run: () => onChangeView("interactive"),
      checked: view === "interactive",
    });
    commands.push({
      id: "view-safe",
      run: () => onChangeView("preview"),
      checked: view === "preview",
    });
  }
  if (doc.format === "md" && !readOnly) {
    // The Markdown editor applies these; the menu just forwards the command id.
    const md = (id: string) => ({
      id,
      run: () => window.dispatchEvent(new CustomEvent(markdownFormatEvent(), { detail: id })),
    });
    commands.push(md("bold"), md("italic"), md("strikethrough"));
    commands.push(md("insert-link"), md("insert-table"), md("insert-image"));
  }
  if (cap.canSwitchView && doc.format === "md" && !readOnly)
    commands.push({
      id: "view-raw-source",
      run: () => onChangeView(view === "source" ? "preview" : "source"),
      checked: view === "source",
    });
  if (cap.canSwitchView && doc.format === "mdx" && !readOnly) {
    commands.push({
      id: "view-preview",
      run: () => onChangeView("preview"),
      checked: view === "preview",
    });
    commands.push({
      id: "view-source",
      run: () => onChangeView("source"),
      checked: view === "source",
    });
  }
  if (editable) {
    commands.push({ id: "spellcheck", run: toggleSpellcheck, checked: spellcheck });
    commands.push({ id: "word-wrap", run: toggleWordWrap, checked: wordWrap });
  }
  return commands;
}

function NativeDocumentMenus(
  props: Pick<
    Props,
    | "doc"
    | "view"
    | "onSave"
    | "onSaveAs"
    | "onPrint"
    | "onClose"
    | "onChangeView"
    | "onEditImageExternal"
    | "docxMenu"
    | "readOnly"
  >,
) {
  const { settings, update } = useSettings();
  useNativeDocumentMenu(
    documentMenuCommands({
      ...props,
      spellcheck: settings.spellcheck,
      wordWrap: settings.wordWrap,
      toggleSpellcheck: () => update({ spellcheck: !settings.spellcheck }),
      toggleWordWrap: () => update({ wordWrap: !settings.wordWrap }),
    }),
  );
  return null;
}

export const DocumentModeControls = memo(DocumentModeControlsImpl);

// Right-side toolbar controls group into 38px capsules (.vr-capsule): frosted panel glass, hairline
// border, soft shadow. The switch inside is a 32px round pill; its active state is the primary
// token at ~14% fill with the accent foreground (text-copper-hi resolves to paper inside chrome).
function DocumentModeControlsImpl({
  doc,
  view,
  onChangeView,
  readOnly = false,
}: Pick<Props, "doc" | "view" | "onChangeView" | "readOnly">) {
  // Read mode: the md "source" button and the mdx preview/source switch only reach editing
  // surfaces — hide them. HTML's scripts ON/OFF knob is a viewing control and stays.
  // Markdown's format bar carries its own Source button, so the title bar only offers the way back
  // to the live editor while the raw source is open. Read-only MDX hides its switch too.
  // One fixed view switch (right capsule of the document bar) in every mode — it never moves
  // between the format bar and the title bar. Read-only MDX hides it.
  const hideViewSwitch = readOnly && doc.format === "mdx";
  return (
    <div className="flex min-w-0 items-center gap-2 font-sans text-chrome-sm text-muted-2">
      {!hideViewSwitch && (
        <ViewSwitch format={doc.format} view={view} onChangeView={onChangeView} />
      )}
      <ScriptsSwitch format={doc.format} view={view} onChangeView={onChangeView} />
    </div>
  );
}

/** the host app own DOCX menu bar (File/Format/Insert) — drives the lifted bridge API, no Eigenpal UI.
 *  No "Open…" here: opening is the app-level brokered Open (DocActionBar/sidebar/Ctrl+O). The vendor's
 *  in-editor picker is intentionally never surfaced — it swapped the editor buffer without changing
 *  the active doc.path, so a later Save would write the new content over the original file. */
function DocxMenus({
  api,
  onSaveAs,
  onClose,
  readOnly = false,
}: {
  api: DocxMenuApi;
  onSaveAs: () => void;
  onClose: () => void;
  readOnly?: boolean;
}) {
  const fmt = (a: Parameters<DocxMenuApi["format"]>[0]) => () => api.format(a);
  return (
    <div className="-ml-2 flex items-center font-sans text-chrome-sm">
      <Dropdown
        variant="menu"
        label="File"
        title={menuTitle("File", "F")}
        panelWidth={176}
        accelerator="f"
      >
        {(close) => (
          <>
            <Item
              hint={shortcutLabel("mod+s")}
              onRun={() => {
                api.save();
                close();
              }}
            >
              Save
            </Item>
            <Item
              hint={shortcutLabel("mod+shift+s")}
              onRun={() => {
                onSaveAs();
                close();
              }}
            >
              Save As…
            </Item>
            <Item
              onRun={() => {
                api.pageSetup();
                close();
              }}
            >
              Page setup…
            </Item>
            <Item
              hint={shortcutLabel("mod+p")}
              onRun={() => {
                api.print();
                close();
              }}
            >
              Print
            </Item>
            <Item
              hint={shortcutLabel("mod+w")}
              onRun={() => {
                onClose();
                close();
              }}
            >
              Close
            </Item>
          </>
        )}
      </Dropdown>
      {/* Format + Insert are pure editing menus — hidden in Read; File (save/print/close) stays. */}
      {!readOnly && (
        <>
          <Dropdown
            variant="menu"
            label="Format"
            title={menuTitle("Format", "O")}
            panelWidth={158}
            accelerator="o"
          >
            {(close) => (
              <>
                <Item
                  hint={shortcutLabel("mod+b")}
                  onRun={() => {
                    fmt("bold")();
                    close();
                  }}
                >
                  Bold
                </Item>
                <Item
                  hint={shortcutLabel("mod+i")}
                  onRun={() => {
                    fmt("italic")();
                    close();
                  }}
                >
                  Italic
                </Item>
                <Item
                  hint={shortcutLabel("mod+u")}
                  onRun={() => {
                    fmt("underline")();
                    close();
                  }}
                >
                  Underline
                </Item>
                <Item
                  onRun={() => {
                    fmt("strikethrough")();
                    close();
                  }}
                >
                  Strikethrough
                </Item>
                <Item
                  onRun={() => {
                    fmt("superscript")();
                    close();
                  }}
                >
                  Superscript
                </Item>
                <Item
                  onRun={() => {
                    fmt("subscript")();
                    close();
                  }}
                >
                  Subscript
                </Item>
                <Item
                  onRun={() => {
                    fmt("clearFormatting")();
                    close();
                  }}
                >
                  Clear formatting
                </Item>
              </>
            )}
          </Dropdown>
          <Dropdown
            variant="menu"
            label="Insert"
            title={menuTitle("Insert", "N")}
            panelWidth={170}
            accelerator="n"
          >
            {(close) => (
              <>
                <Item
                  onRun={() => {
                    api.insertTable(3, 3);
                    close();
                  }}
                >
                  Table (3 × 3)
                </Item>
                <Item
                  onRun={() => {
                    api.insertImage();
                    close();
                  }}
                >
                  Image…
                </Item>
                <Item
                  onRun={() => {
                    fmt("insertLink")();
                    close();
                  }}
                >
                  Link…
                </Item>
                <Item
                  onRun={() => {
                    api.insertPageBreak();
                    close();
                  }}
                >
                  Page break
                </Item>
                <Item
                  onRun={() => {
                    api.insertTOC();
                    close();
                  }}
                >
                  Table of contents
                </Item>
              </>
            )}
          </Dropdown>
        </>
      )}
    </div>
  );
}

/** Menu bar for text + pdf formats. File always; Edit for editable text; View for render-switchable. */
function TextMenus({
  doc,
  view,
  onSave,
  onSaveAs,
  onPrint,
  onClose,
  onChangeView,
  onEditImageExternal,
  readOnly = false,
}: {
  doc: LoadedDocument;
  view: ViewMode;
  onSave: () => void;
  onSaveAs: () => void;
  onPrint: () => void;
  onClose: () => void;
  onChangeView: (v: ViewMode) => void;
  onEditImageExternal?: () => void;
  readOnly?: boolean;
}) {
  const { settings, update } = useSettings();
  const cap = capabilitiesFor(doc.format);
  // Text-only Edit menu: image is saveable but edits in its own viewer shell, so canReplace
  // (txt/md/mdx/html) is the right gate here, not canEdit.
  const editable = cap.canReplace;
  const saveable = cap.canEdit;
  const saveAs = cap.canSaveAs;
  const viewSwitch = cap.canSwitchView;
  // execCommand acts on the focused source editor; the menu items are
  // focus-safe (onMouseDown), so the editor keeps focus + selection while the menu is used.
  const exec = (cmd: string) => () => document.execCommand(cmd);
  const dispatchEditorCommand = (type: string) => () => {
    window.dispatchEvent(new Event(type, { cancelable: true }));
  };
  const selectAll = () => {
    if (doc.format === "html") {
      const event = new Event(documentEvent("html-select-all"), { cancelable: true });
      window.dispatchEvent(event);
      if (event.defaultPrevented) return;
    }
    document.execCommand("selectAll");
  };
  return (
    <div className="-ml-2 flex items-center font-sans text-chrome-sm">
      <Dropdown
        variant="menu"
        label="File"
        title={menuTitle("File", "F")}
        panelWidth={176}
        accelerator="f"
      >
        {(close) => (
          <>
            {/* No "Open…" here — opening is always available from the sidebar / Ctrl+O. */}
            {saveable && (
              <Item
                hint={shortcutLabel("mod+s")}
                onRun={() => {
                  onSave();
                  close();
                }}
              >
                Save
              </Item>
            )}
            {saveAs && (
              <Item
                hint={shortcutLabel("mod+shift+s")}
                onRun={() => {
                  onSaveAs();
                  close();
                }}
              >
                Save As…
              </Item>
            )}
            {doc.format === "image" && onEditImageExternal && (
              <Item
                onRun={() => {
                  onEditImageExternal();
                  close();
                }}
              >
                Edit image externally…
              </Item>
            )}
            <Item
              hint={shortcutLabel("mod+p")}
              onRun={() => {
                onPrint();
                close();
              }}
            >
              Print
            </Item>
            <Item
              hint={shortcutLabel("mod+w")}
              onRun={() => {
                onClose();
                close();
              }}
            >
              Close
            </Item>
          </>
        )}
      </Dropdown>
      {/* The whole Edit menu (undo/cut/paste) is an editing surface — hidden in Read. */}
      {editable && !readOnly && (
        <Dropdown
          variant="menu"
          label="Edit"
          title={menuTitle("Edit", "E")}
          panelWidth={140}
          accelerator="e"
        >
          {(close) => (
            <>
              <Item
                hint={shortcutLabel("mod+z")}
                onRun={() => {
                  dispatchEditorCommand(documentEvent("undo"))();
                  close();
                }}
              >
                Undo
              </Item>
              <Item
                hint={shortcutLabel("mod+y")}
                onRun={() => {
                  dispatchEditorCommand(documentEvent("redo"))();
                  close();
                }}
              >
                Redo
              </Item>
              <Item
                hint={shortcutLabel("mod+x")}
                onRun={() => {
                  exec("cut")();
                  close();
                }}
              >
                Cut
              </Item>
              <Item
                hint={shortcutLabel("mod+c")}
                onRun={() => {
                  exec("copy")();
                  close();
                }}
              >
                Copy
              </Item>
              <Item
                hint={shortcutLabel("mod+v")}
                onRun={() => {
                  exec("paste")();
                  close();
                }}
              >
                Paste
              </Item>
              <Item
                hint={shortcutLabel("mod+a")}
                onRun={() => {
                  selectAll();
                  close();
                }}
              >
                Select all
              </Item>
            </>
          )}
        </Dropdown>
      )}
      {/* View owns the render mode (when there is one) plus Spell check + Word wrap for ALL editable
          text — so plain txt, md, and html toggle them in the same place (consistent home). */}
      {(viewSwitch || editable) && (
        <Dropdown
          variant="menu"
          label="View"
          title={menuTitle("View", "V")}
          panelWidth={150}
          accelerator="v"
        >
          {(close) => (
            <>
              {viewSwitch && doc.format === "html" && (
                <>
                  {/* Interactive = scripts + in-page nav (default); Safe = scriptless no-network render.
                      Source is the in-place drawer (the `source` button in the page chrome), not a mode. */}
                  <Item
                    active={view === "interactive"}
                    onRun={() => {
                      onChangeView("interactive");
                      close();
                    }}
                  >
                    Interactive
                  </Item>
                  <Item
                    active={view === "preview"}
                    onRun={() => {
                      onChangeView("preview");
                      close();
                    }}
                  >
                    Safe
                  </Item>
                </>
              )}
              {/* Raw-source items reach the editing drawer — hidden in Read. HTML's
                  Interactive/Safe pair is a viewing control and stays. */}
              {viewSwitch && doc.format === "md" && !readOnly && (
                <Item
                  active={view === "source"}
                  onRun={() => {
                    onChangeView(view === "source" ? "preview" : "source");
                    close();
                  }}
                >
                  {view === "source" ? "Return to live editor" : "Raw source"}
                </Item>
              )}
              {viewSwitch && doc.format === "mdx" && !readOnly && (
                <>
                  <Item
                    active={view === "preview"}
                    onRun={() => {
                      onChangeView("preview");
                      close();
                    }}
                  >
                    Preview
                  </Item>
                  <Item
                    active={view === "source"}
                    onRun={() => {
                      onChangeView("source");
                      close();
                    }}
                  >
                    Source
                  </Item>
                </>
              )}
              {editable && (
                <>
                  <Item
                    active={settings.spellcheck}
                    onRun={() => {
                      update({ spellcheck: !settings.spellcheck });
                      close();
                    }}
                  >
                    Spell check
                  </Item>
                  <Item
                    active={settings.wordWrap}
                    onRun={() => {
                      update({ wordWrap: !settings.wordWrap });
                      close();
                    }}
                  >
                    Word wrap
                  </Item>
                </>
              )}
            </>
          )}
        </Dropdown>
      )}
    </div>
  );
}

/** Markdown keeps one live editor; raw source is its fallback drawer. MDX retains its preview/source
 *  switch because executable MDX cannot mount in the app's editable React tree. */
function ViewSwitch({
  format,
  view,
  onChangeView,
}: {
  format: LoadedDocument["format"];
  view: ViewMode;
  onChangeView: (v: ViewMode) => void;
}) {
  // Segmented two-way switch that stays in one place: Markdown Editor | Source, MDX Preview | Source.
  if (format !== "md" && format !== "mdx") return null;
  const first = format === "md" ? "Editor" : "Preview";
  const sourceOpen = view === "source";
  const seg = (on: boolean) =>
    `vr-press vr-focus h-7 rounded-full px-2.5 ${
      on ? "bg-copper/[0.14] text-copper-hi" : "text-muted-2 hover:bg-ink/40 hover:text-paper"
    }`;
  return (
    <div role="group" aria-label="View" className="flex items-center gap-0.5 select-none">
      <button
        type="button"
        aria-pressed={!sourceOpen}
        aria-label={format === "md" ? "Show live editor" : "Show MDX preview"}
        title={format === "md" ? "Live editor" : "Preview"}
        onClick={() => onChangeView("preview")}
        className={seg(!sourceOpen)}
      >
        {first}
      </button>
      <button
        type="button"
        aria-pressed={sourceOpen}
        aria-label={format === "md" ? "Show raw Markdown source" : "Show MDX source"}
        title={format === "md" ? "Raw Markdown source" : "MDX source"}
        onClick={() => onChangeView("source")}
        className={seg(sourceOpen)}
      >
        Source
      </button>
    </div>
  );
}

/** HTML scripts ON/OFF toggle (Interactive ↔ Safe), md-switch styling: label + sliding knob + state.
 *  ON (Interactive, page JS runs) is RED; OFF (Safe, scriptless render) is GREEN. Lives here in the
 *  action bar — same place/feel as md's preview/source switch — not in the page URL chrome. Returns
 *  null for every non-HTML format. HTML's `view` is only ever "interactive" or "preview" (the source
 *  drawer is separate internal state), so `view !== "preview"` cleanly means scripts ON. */
function ScriptsSwitch({
  format,
  view,
  onChangeView,
}: {
  format: LoadedDocument["format"];
  view: ViewMode;
  onChangeView: (v: ViewMode) => void;
}) {
  if (format !== "html") return null;
  // Mirror HtmlDocument's `mode` (view === "preview" ? Safe : Interactive) exactly, so the knob never
  // disagrees with what the preview is actually doing — a stale/odd ViewMode reads as Interactive in
  // both places. Red dot = ON (scripts run), green = OFF (Safe); the colour + position carry state.
  const interactive = view !== "preview";
  return (
    <div
      className="flex items-center gap-2 px-1.5 select-none"
      title={
        interactive
          ? `Scripts ON — the page runs like a browser tab (remote scripts, fonts, network), but in an origin-isolated sandbox that can't touch ${hostAppName()} or your files. Click for Safe (scriptless render).`
          : "Scripts OFF — Safe scriptless render. Click to run the page (Interactive)."
      }
    >
      <span className="text-muted-2">scripts</span>
      <button
        type="button"
        role="switch"
        aria-checked={interactive}
        aria-label={
          interactive
            ? "Scripts on (Interactive). Switch to Safe."
            : "Scripts off (Safe). Switch to Interactive."
        }
        onClick={() => onChangeView(interactive ? "preview" : "interactive")}
        className={`vr-focus flex h-4 w-8 shrink-0 items-center rounded-full border px-0.5 ${
          interactive
            ? "justify-end border-[color:color-mix(in_srgb,var(--danger)_60%,var(--line))] bg-[color:color-mix(in_srgb,var(--danger)_20%,transparent)]"
            : "justify-start border-[color:color-mix(in_srgb,var(--accent-2)_60%,var(--line))] bg-[color:color-mix(in_srgb,var(--accent-2)_20%,transparent)]"
        }`}
      >
        <span
          className={`h-2.5 w-2.5 rounded-full transition-all duration-150 ${interactive ? "bg-danger-hi" : "bg-accent2"}`}
        />
      </button>
    </div>
  );
}

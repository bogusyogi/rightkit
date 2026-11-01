// Label catalog mutations (create / rename / delete / recolor) and opening a
// custom-label view. Extracted from App. Each mutation hits Gmail then reloads
// the active account's catalog.
import { useAppStore } from "../store";
import { showDialog } from "./dialog";
import { setPref } from "../lib/preferences";
import {
  listLabels,
  gmailCreateLabel,
  gmailRenameLabel,
  gmailDeleteLabel,
  gmailSetLabelColor,
  relabel,
  type ThreadRef,
} from "../lib/ipc";

export function useLabelActions() {
  const activeAccount = useAppStore((s) => s.activeAccount);
  const labels = useAppStore((s) => s.labels);
  const activeLabel = useAppStore((s) => s.activeLabel);
  const setLabels = useAppStore((s) => s.setLabels);
  const setErr = useAppStore((s) => s.setErr);
  const setActiveLabel = useAppStore((s) => s.setActiveLabel);
  const setLabelColors = useAppStore((s) => s.setLabelColors);
  const setCategory = useAppStore((s) => s.setCategory);
  const setView = useAppStore((s) => s.setView);
  const setOpenThread = useAppStore((s) => s.setOpenThread);

  /** Open a custom label view (clears folder highlight; label takes precedence). */
  function openLabel(id: string) {
    setActiveLabel(id);
    setCategory(null);
    setView("inbox");
    setOpenThread(null);
  }
  /** Reload the active account's label catalog (after a create/rename/delete). */
  function reloadLabels() {
    if (activeAccount) listLabels(activeAccount).then(setLabels).catch(() => {});
  }
  /** Create a custom label from a prompt ("Parent/Child" nests). */
  async function createLabelUi() {
    if (!activeAccount) return;
    const name = await showDialog({ kind: "prompt", message: 'New label name (use Parent/Child to nest)' });
    if (!name || !name.trim()) return;
    gmailCreateLabel(activeAccount, name.trim())
      .then(reloadLabels)
      .catch((e) => setErr(String(e)));
  }
  async function renameLabelUi(labelId: string) {
    if (!activeAccount) return;
    const cur = labels.find((l) => l.id === labelId)?.name ?? "";
    const name = await showDialog({ kind: "prompt", message: "Rename label", initial: cur });
    if (!name || !name.trim() || name.trim() === cur) return;
    gmailRenameLabel(activeAccount, labelId, name.trim())
      .then(reloadLabels)
      .catch((e) => setErr(String(e)));
  }
  async function deleteLabelUi(labelId: string) {
    if (!activeAccount) return;
    const cur = labels.find((l) => l.id === labelId)?.name ?? "this label";
    if (
      (await showDialog({ kind: "confirm", message: `Delete "${cur}"? It's removed from all messages — the messages themselves stay.` })) !== "ok"
    )
      return;
    gmailDeleteLabel(activeAccount, labelId)
      .then(() => {
        if (activeLabel === labelId) setActiveLabel(null);
        reloadLabels();
      })
      .catch((e) => setErr(String(e)));
  }
  /** Apply a Gmail-palette color to a label (Gmail) + remember it locally for display. */
  function setLabelColorUi(labelId: string, bg: string, text: string) {
    if (!activeAccount) return;
    gmailSetLabelColor(activeAccount, labelId, bg, text).catch((e) => setErr(String(e)));
    setLabelColors((prev) => {
      const next = { ...prev, [labelId]: bg };
      setPref("labelColors", next);
      return next;
    });
  }

  function dropThreadOnLabel(ref: ThreadRef, labelId: string, labelName: string) {
    relabel([ref], [labelId], [])
      .then(() => {
        useAppStore.getState().showUndo(`Added label ${labelName}`, [ref], `unlabel:${labelId}`);
        useAppStore.getState().loadThreads();
      })
      .catch((e) => setErr(String(e)));
  }

  return {
    openLabel,
    reloadLabels,
    createLabelUi,
    renameLabelUi,
    deleteLabelUi,
    setLabelColorUi,
    dropThreadOnLabel,
  };
}

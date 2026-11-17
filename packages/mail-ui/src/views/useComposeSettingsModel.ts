import type { AppState } from "../store/types.js";
import { cancelOutbox, listOutbox, setSignature } from "../lib/ipc/compose.js";
import { setPref } from "../lib/preferences.js";

/** Compose settings state & actions, without presentation. */
export function useComposeSettingsModel(useStore: <T>(selector: (state: AppState) => T) => T) {
  const activeAccount = useStore((s) => s.activeAccount);
  const outbox = useStore((s) => s.outbox);
  const setOutbox = useStore((s) => s.setOutbox);
  const setErr = useStore((s) => s.setErr);
  const signature = useStore((s) => s.signature);
  const setSignatureState = useStore((s) => s.setSignatureState);
  const aliases = useStore((s) => s.aliases);
  const sigAlias = useStore((s) => s.sigAlias);
  const setSigAlias = useStore((s) => s.setSigAlias);
  const aliasSigs = useStore((s) => s.aliasSigs);
  const setAliasSigs = useStore((s) => s.setAliasSigs);
  const snippets = useStore((s) => s.snippets);
  const setSnippets = useStore((s) => s.setSnippets);
  const snipName = useStore((s) => s.snipName);
  const snipBody = useStore((s) => s.snipBody);
  const setSnipName = useStore((s) => s.setSnipName);
  const setSnipBody = useStore((s) => s.setSnipBody);
  const aliasKey = sigAlias || aliases[0]?.email || "";

  const cancelQueuedSend = (id: string) => {
    void cancelOutbox(id).then(() => listOutbox().then(setOutbox)).catch((e) => setErr(String(e)));
  };
  const saveSignature = () => {
    if (activeAccount) void setSignature(activeAccount, signature).catch((e) => setErr(String(e)));
  };
  const saveAliasSignature = (text: string) => {
    const next = { ...aliasSigs, [aliasKey]: text };
    setAliasSigs(next);
    setPref("aliasSigs", next);
  };
  const persistSnippets = (next: typeof snippets) => {
    setSnippets(next);
    setPref("snippets", next);
  };
  const deleteSnippet = (index: number) => persistSnippets(snippets.filter((_, i) => i !== index));
  const addSnippet = () => {
    if (!snipName.trim() || !snipBody.trim()) return;
    persistSnippets([...snippets, { name: snipName.trim(), body: snipBody }]);
    setSnipName("");
    setSnipBody("");
  };
  return {
    activeAccount, outbox, signature, setSignatureState, aliases, aliasKey, setSigAlias,
    aliasSignature: aliasSigs[aliasKey] ?? "", snippets, snipName, snipBody, setSnipName,
    setSnipBody, cancelQueuedSend, saveSignature, saveAliasSignature, deleteSnippet, addSnippet,
  };
}

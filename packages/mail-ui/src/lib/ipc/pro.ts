import { invoke } from "./invoke";

export interface TranslateResult {
  translated_text: string;
  detected_source: string;
}

export function translateText(
  provider: string,
  text: string,
  target: string
): Promise<TranslateResult> {
  return invoke<TranslateResult>("translate_text", { provider, text, target });
}

export function setTranslateKey(provider: string, key: string): Promise<void> {
  return invoke<void>("set_translate_key", { provider, key });
}

export type AiAction =
  | "proofread"
  | "shorter"
  | "warmer"
  | "more_direct"
  | "summarize_thread"
  | "draft_reply";

export function aiRun(action: AiAction, text: string): Promise<string> {
  return invoke<string>("ai_run", { action, text });
}

export function setAiKey(key: string): Promise<void> {
  return invoke<void>("set_ai_key", { key });
}

export function setAiConfig(baseUrl: string, model: string): Promise<void> {
  return invoke<void>("set_ai_config", { baseUrl, model });
}

export type TrustLevel = "info" | "warn" | "danger";

export interface TrustFinding {
  level: TrustLevel;
  message: string;
}

export interface TrustReport {
  level: TrustLevel;
  findings: TrustFinding[];
}

export function trustCheck(
  fromAddr: string,
  fromName: string,
  replyTo: string | null,
  bodyHtml: string
): Promise<TrustReport> {
  return invoke<TrustReport>("trust_check", { fromAddr, fromName, replyTo, bodyHtml });
}

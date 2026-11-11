// The in-frame MDX runtime lives in mdxRuntimeScript.txt and is injected into the sandboxed iframe.
// This module owns only the protocol-token substitution that must stay in sync with mdxProtocol.ts.

import runtimeScript from "./mdxRuntimeScript.js";
import {
  MDX_COMPILED,
  MDX_FIND,
  MDX_FIND_CLEAR,
  MDX_FIND_OPEN,
  MDX_FIND_RESULT,
  MDX_PRINT,
  MDX_PRINT_CONTENT,
  MDX_FIND_STEP,
  MDX_HEIGHT,
  MDX_LINK,
  MDX_RUNTIME_ERROR,
  MDX_THEME,
} from "./mdxProtocol.js";

const PROTOCOL_TOKENS: Array<[string, string]> = [
  ['"__VR_MDX_COMPILED__"', MDX_COMPILED],
  ['"__VR_MDX_LINK__"', MDX_LINK],
  ['"__VR_MDX_RUNTIME_ERROR__"', MDX_RUNTIME_ERROR],
  ['"__VR_MDX_HEIGHT__"', MDX_HEIGHT],
  ['"__VR_MDX_FIND_OPEN__"', MDX_FIND_OPEN],
  ['"__VR_MDX_FIND__"', MDX_FIND],
  ['"__VR_MDX_FIND_STEP__"', MDX_FIND_STEP],
  ['"__VR_MDX_FIND_CLEAR__"', MDX_FIND_CLEAR],
  ['"__VR_MDX_FIND_RESULT__"', MDX_FIND_RESULT],
  ['"__VR_MDX_PRINT__"', MDX_PRINT],
  ['"__VR_MDX_PRINT_CONTENT__"', MDX_PRINT_CONTENT],
  ['"__VR_MDX_THEME__"', MDX_THEME],
];

export function mdxBridgeScript(sessionId: string): string {
  let script = runtimeScript.replace('"__VR_SESSION_ID__"', JSON.stringify(sessionId));
  for (const [token, value] of PROTOCOL_TOKENS) {
    script = script.replace(token, JSON.stringify(value));
  }
  return script;
}

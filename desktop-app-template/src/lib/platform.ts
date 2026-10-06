// Platform authority is @rightkit/platform-ui; browser QA opts in with ?qa=1&qaPlatform=windows|macos.
import { bootstrapQaPlatform, getPlatform, type Platform } from "@rightkit/platform-ui";

export function detectPlatform(): Platform {
  bootstrapQaPlatform({ query: globalThis.location?.search ?? "" });
  return getPlatform();
}

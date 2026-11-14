import { hostOnboarding, type DictationOnboardingModel } from "../host.js";

/** The model the setup installs, from the host configuration. */
export function onboardingModel(): DictationOnboardingModel {
  return hostOnboarding().model;
}

/** The spoken word that starts the voice commands. */
export function onboardingWakeWord(): string {
  return hostOnboarding().wakeWord;
}

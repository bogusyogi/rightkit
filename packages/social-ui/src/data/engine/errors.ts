// Errors the engine data source throws from reads. Writes never throw: they return a Result.

/**
 * A read the engine cannot answer yet, because the method or the field does not exist in the engine.
 * Screens show their load error state for it. The message is kept for logs and tests.
 */
export class EngineUnavailableError extends Error {
  readonly code = "engine_unavailable" as const;

  constructor(message: string) {
    super(message);
    this.name = "EngineUnavailableError";
  }
}

/** A read the engine answered with an error, or an engine reply the desktop cannot map. */
export class EngineReadError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = "EngineReadError";
    this.code = code;
  }
}

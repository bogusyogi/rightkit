/** Revision-safe async Markdown validation. Native filesystem access is not
 * part of this path: KaTeX/Mermaid validation receives source text only. */
export type MarkdownAsyncValidator<T, Result> = (value: T) => Promise<Result>;

export interface MarkdownValidationResult<Result> {
  readonly revision: number;
  readonly value: Result;
}

/** Monotonic requests prevent a slow prior revision from overwriting current UI. */
export class MarkdownValidationOrchestrator<T, Result> {
  private latestRequest = 0;

  constructor(private readonly validator: MarkdownAsyncValidator<T, Result>) {}

  async validate(revision: number, value: T): Promise<MarkdownValidationResult<Result> | null> {
    const request = ++this.latestRequest;
    const result = await this.validator(value);
    return request === this.latestRequest ? { revision, value: result } : null;
  }

  invalidate(): void {
    this.latestRequest++;
  }
}

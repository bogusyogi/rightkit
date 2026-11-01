/** Centered title + body filling the viewer area — shown when a preview can't render (missing
 *  session metadata, self-framing blocked, build/render error). Shared by the HTML preview chrome
 *  and the Live-Preview pane so the empty/error state reads identically across both. */
export function PreviewMessage({ title, body }: { title: string; body: string }) {
  return (
    <div className="flex h-full flex-col items-center justify-center px-6 text-center">
      <p className="font-serif text-xl font-semibold text-paper">{title}</p>
      <p className="mt-2 max-w-xl text-sm text-muted">{body}</p>
    </div>
  );
}

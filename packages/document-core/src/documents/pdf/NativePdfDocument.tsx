import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useRef,
  useState,
  type ComponentProps,
} from "react";
import { isMac } from "../../platform.js";
import { logError } from "../../state/diagnostics.js";
import type { ViewMeta } from "../types.js";
import type { PdfViewer as LegacyPdfViewer } from "./PdfViewer.js";
import type { NativePdfEditingAction } from "./macPdfNativeBridge.js";

const MacPdfReadPane = lazy(() =>
  import("./MacPdfReadPane.js").then((module) => ({ default: module.MacPdfReadPane })),
);
const PdfViewer = lazy(() =>
  import("./PdfViewer.js").then((module) => ({ default: module.PdfViewer })),
);

export function supportsNativePdfReading(): boolean {
  return isMac() && typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

type Props = ComponentProps<typeof LegacyPdfViewer>;

/** Native reading & PDF.js editing have separate document lifetimes. Once editing starts, keep
 * its renderer mounted so annotations, source commands & undo history retain their existing owner.
 */
export function NativePdfDocument(props: Props) {
  const readOnly = props.readOnly ?? true;
  const [legacy, setLegacy] = useState(() => !supportsNativePdfReading());
  const [editingAction, setEditingAction] = useState<NativePdfEditingAction>();
  // Wave 2: Markup hands off to the PDF.js editor permanently for this document — the editor's
  // document lifetime rule (annotations, source commands & undo keep their existing owner) makes
  // a return to the native reader destructive, so Read after Markup renders the same editor in
  // its read-only mode instead of remounting PDFKit.
  useEffect(() => {
    if (!readOnly) setLegacy(true);
  }, [readOnly]);
  const showNative = !legacy && readOnly;
  const viewRef = useRef<ViewMeta | undefined>(props.initialView);
  const onViewState = useCallback(
    (view: ViewMeta) => {
      viewRef.current = view;
      props.onViewState?.(view);
    },
    [props.onViewState],
  );
  const requestEditing = useCallback((action: NativePdfEditingAction, view: ViewMeta) => {
    viewRef.current = view;
    setEditingAction(action);
    setLegacy(true);
  }, []);
  const fallback = useCallback(() => {
    logError("pdf:native-reading-failed");
    setLegacy(true);
  }, []);

  return (
    <Suspense
      fallback={
        <div className="flex h-full items-center justify-center text-sm text-muted">
          Loading PDF…
        </div>
      }
    >
      {showNative ? (
        <MacPdfReadPane
          key={props.url ?? props.documentPath}
          documentPath={props.documentPath}
          url={props.url}
          active={props.active}
          initialView={viewRef.current}
          onViewState={onViewState}
          registerPrint={props.registerPrint}
          registerSearchProvider={props.registerSearchProvider}
          findBar={props.findBar}
          findMode={props.findMode}
          onRequestFind={props.onRequestFind}
          onRequestReplace={props.onRequestReplace}
          onRequestEditing={requestEditing}
          onRequestFallback={fallback}
          readOnly
        />
      ) : (
        <PdfViewer
          {...props}
          readOnly={readOnly}
          initialView={viewRef.current}
          initialEditingAction={editingAction}
          onViewState={onViewState}
        />
      )}
    </Suspense>
  );
}

import { useRef, useState, type DragEvent } from "react";

const hasFileDrag = (types: DataTransfer["types"] | null | undefined) => {
  if (!types) return false;
  if (typeof types.includes === "function") return types.includes("Files");
  const domStringList = types as { contains?: (type: string) => boolean };
  return typeof domStringList.contains === "function" && domStringList.contains("Files");
};

/** Root-level file-drag "Drop to open" overlay state + handlers, extracted from App.tsx. */
export function useFileDragOverlay() {
  const [dragOver, setDragOver] = useState(false);
  const dragCounter = useRef(0);
  const rootDragProps = {
    onDragEnter: (e: DragEvent) => {
      if (hasFileDrag(e.dataTransfer?.types)) {
        e.preventDefault();
        dragCounter.current++;
        setDragOver(true);
      }
    },
    onDragOver: (e: DragEvent) => {
      if (hasFileDrag(e.dataTransfer?.types)) {
        e.preventDefault();
        if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
      }
    },
    onDragLeave: (e: DragEvent) => {
      if (dragOver || hasFileDrag(e.dataTransfer?.types)) {
        e.preventDefault();
        dragCounter.current--;
        if (dragCounter.current <= 0) {
          dragCounter.current = 0;
          setDragOver(false);
        }
      }
    },
    onDrop: () => {
      dragCounter.current = 0;
      setDragOver(false);
    },
  };
  return { dragOver, rootDragProps };
}

export function DropOverlay() {
  return (
    <div className="fixed inset-0 z-[100] pointer-events-none flex items-center justify-center bg-copper/10 border-2 border-dashed border-copper/50">
      <div className="rounded-lg bg-panel-2 px-6 py-4 shadow-2xl border border-copper/30 font-sans text-sm text-copper-hi">
        Drop to open
      </div>
    </div>
  );
}

// streamingStore — narrow slice for token-append state.
//
// Lives apart from useSessionEvents so streaming consumers (transcript subtree
// + composer status) can subscribe per-field. Sidebar, title bar, settings,
// and the rest of App.tsx never subscribe to this slice and therefore do not
// re-render on every streamed token.
//
// All accessors are stable refs; updates mutate the slice atomically.

import { create } from "zustand";

export interface StreamingStoreState {
  streamingText: string;
  streamingReasoning: string;
  reasoningExpanded: boolean;
  streamingToolName: string | null;
  appendText: (chunk: string) => void;
  appendReasoning: (chunk: string) => void;
  setReasoningExpanded: (value: boolean | ((prev: boolean) => boolean)) => void;
  setStreamingToolName: (value: string | null) => void;
  clear: () => void;
}

export const useStreamingStore = create<StreamingStoreState>((set) => ({
  streamingText: "",
  streamingReasoning: "",
  reasoningExpanded: false,
  streamingToolName: null,
  appendText: (chunk) => set((state) => ({ streamingText: state.streamingText + chunk })),
  appendReasoning: (chunk) =>
    set((state) => ({ streamingReasoning: state.streamingReasoning + chunk })),
  setReasoningExpanded: (value) =>
    set((state) => ({
      reasoningExpanded:
        typeof value === "function"
          ? (value as (prev: boolean) => boolean)(state.reasoningExpanded)
          : value,
    })),
  setStreamingToolName: (value) => set({ streamingToolName: value }),
  clear: () => set({ streamingText: "", streamingReasoning: "", streamingToolName: null }),
}));

// Remote-image consent flow: when the user allows images for an open thread,
// each <img src=http…> is fetched through Rust (cached, served via the
// mailright-img:// protocol) instead of letting the iframe hit the network.
// Extracted from App. Owns the in-flight de-dupe set.
import { useEffect, useRef } from "react";
import { useAppStore } from "../store/index.js";
import { fetchRemoteImage } from "../lib/ipc.js";
import { MAX_REMOTE_IMAGES_PER_MESSAGE, remoteImageSrc } from "../lib/imageTrust.js";

export function useRemoteImages() {
  const showImages = useAppStore((s) => s.showImages);
  const openThread = useAppStore((s) => s.openThread);
  const threadMsgs = useAppStore((s) => s.threadMsgs);
  const expandedMsgs = useAppStore((s) => s.expandedMsgs);
  const remoteImgMap = useAppStore((s) => s.remoteImgMap);
  const setRemoteImgMap = useAppStore((s) => s.setRemoteImgMap);
  const setRemoteImgBlockedCount = useAppStore((s) => s.setRemoteImgBlockedCount);
  const remoteImgInFlight = useRef<Set<string>>(new Set());

  function loadRemoteImages(messageId: string, html: string) {
    const urls = new Set<string>();
    const re = /<img\b[^>]*?\bsrc\s*=\s*["']?(https?:\/\/[^"'\s>]+)/gi;
    let m: RegExpExecArray | null;
    while ((m = re.exec(html))) {
      urls.add(m[1]);
      if (urls.size >= MAX_REMOTE_IMAGES_PER_MESSAGE) break;
    }
    if (urls.size === 0) return;
    const have = remoteImgMap[messageId] || {};
    for (const url of urls) {
      if (have[url]) continue; // already cached this session
      const key = `${messageId}\n${url}`;
      if (remoteImgInFlight.current.has(key)) continue;
      remoteImgInFlight.current.add(key);
      fetchRemoteImage(url)
        .then((img) => {
          setRemoteImgMap((prev) => ({
              ...prev,
              [messageId]: {
                ...(prev[messageId] || {}),
                // Tauri returns a stable cached-image hash; browser QA returns a
                // self-contained data: URL because there is no Rust image protocol.
                [url]: remoteImageSrc(img.hash),
              },
            }));
          if (img.blocked) {
            setRemoteImgBlockedCount((prev) => ({
              ...prev,
              [messageId]: (prev[messageId] || 0) + 1,
            }));
          }
        })
        .catch(() => {})
        .finally(() => {
          remoteImgInFlight.current.delete(key);
        });
    }
  }

  // When remote images are consented to, fetch + cache them in Rust so they
  // render from the local cache and don't re-download on every open.
  useEffect(() => {
    if (!showImages || !openThread) return;
    threadMsgs.forEach((m, i) => {
      if (!expandedMsgs.has(i)) return;
      if (m.is_html && m.body.includes("http")) loadRemoteImages(m.message_id, m.body);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps -- loadRemoteImages is a plain function redefined every render (it closes over remoteImgMap); tracking it would refetch already-cached images on every render instead of only when the thread/consent/expansion inputs change.
  }, [showImages, threadMsgs, openThread, expandedMsgs]);
}

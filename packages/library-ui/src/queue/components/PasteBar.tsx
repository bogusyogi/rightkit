import { useCallback, useMemo, useRef, useState } from "react";
import { AudioLines, Check, Images, Link2, Plus, Text, Video } from "lucide-react";
import { useSegmentedControl } from "@rightkit/react-shell/headless";
import type { AudioQuality, MediaSpec, VideoQuality } from "../contract.js";
import type { ProfileImagesResponse } from "../contract.js";
import { formatInvokeError } from "../lib/errorMessage.js";
import { invoke } from "../lib/ipc/index.js";
import { isPlaylistUrl, playlistQueuedNotice, type PlaylistResponse } from "../lib/playlistUrl.js";
import {
  imageSpecForInputs,
  isProfileUrl,
  mediaKindForInputs,
  mediaSpecForMode,
  needsAuthPlatform,
  splitImageLines,
  type MediaMode,
} from "../lib/mediaSelection.js";
import { loadPrefs } from "../views/settings/prefs.js";
import { useLicense } from "../views/settings/useLicense.js";
import { hostAppName } from "../../appName.js";

type PasteBarProps = {
  onAdd: (inputs: string[], media?: MediaSpec) => Promise<boolean> | boolean;
  onPickFiles: () => void;
  submitBlocked?: boolean;
  /** Free-tier click on the + Transcript pill → open Settings at the Pro
   *  section (combined download+transcript is a Pro surface). */
  onOpenPro?: () => void;
};
const OUTPUT_MODES = [
  { value: "transcribe", label: "Transcribe", Icon: Text },
  { value: "audio", label: "Audio", Icon: AudioLines },
  { value: "video", label: "Video + audio", Icon: Video },
  { value: "images", label: "Save images", Icon: Images },
] as const;

export function PasteBar({
  onAdd,
  onPickFiles,
  submitBlocked = false,
  onOpenPro,
}: PasteBarProps) {
  const draftRef = useRef<HTMLTextAreaElement>(null);
  const [draft, setDraft] = useState("");
  const [focused, setFocused] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [profileStatus, setProfileStatus] = useState<string | null>(null);
  const [profileError, setProfileError] = useState<string | null>(null);
  const [authPrompt, setAuthPrompt] = useState<{ platform: string; url: string } | null>(null);
  // WS-5: "+ Transcript" upgrades an audio/video download to action:"both".
  const [withTranscript, setWithTranscript] = useState(false);
  const [mediaMode, setMediaMode] = useState<MediaMode>(() => loadPrefs().defaultOutput);
  const [videoQuality, setVideoQuality] = useState<VideoQuality>(
    () => loadPrefs().defaultVideoQuality,
  );
  const [audioQuality, setAudioQuality] = useState<AudioQuality>(
    () => loadPrefs().defaultAudioQuality,
  );
  const { pro } = useLicense();
  const source = useMemo(() => mediaKindForInputs(draft), [draft]);
  // Free tier caps video downloads at 1080p — 4K/source stay visible but
  // Pro-marked, matching the Downloads & Files settings row.
  const effectiveVideoQuality: VideoQuality =
    !pro && (videoQuality === "p2160" || videoQuality === "source")
      ? "p1080"
      : videoQuality;
  const unavailable = (mode: MediaMode) => {
    if (!draft.trim()) return false;
    // "Save images" needs at least one image-capable line; mixed drafts
    // save images where they can and transcribe the rest (splitImageLines).
    if (mode === "images") return splitImageLines(draft).image.length === 0;
    return (
      mode !== "transcribe" &&
      (source.kind === "local" ||
        (mode === "video" && (source.kind === "audio" || source.kind === "mixed")))
    );
  };
  const modeUnavailable = unavailable(mediaMode);
  const outputSegments = useSegmentedControl({
    value: mediaMode,
    options: OUTPUT_MODES.map(({ value }) => ({ value, disabled: unavailable(value) })),
    onChange: setMediaMode,
    label: "Output",
    disabled: submitBlocked,
  });
  const help = modeUnavailable
    ? source.kind === "local"
      ? "Downloads require a media link."
      : "This source provides audio only. Choose Audio."
    : null;
  /** Send one profile URL for a batch save. Returns true when the engine
   * accepted it; a needs_auth reply opens the sign-in prompt instead. */
  const sendProfile = useCallback(async (url: string): Promise<boolean> => {
    try {
      const response = await invoke<ProfileImagesResponse>("submit_profile_images", { url });
      setAuthPrompt(null);
      setProfileError(null);
      setProfileStatus(`Queued ${response.accepted} posts from ${response.profile}`);
      return true;
    } catch (error) {
      const message = formatInvokeError(error);
      const platform = needsAuthPlatform(message);
      setProfileStatus(null);
      if (platform) {
        setAuthPrompt({ platform, url });
        setProfileError(null);
      } else {
        setAuthPrompt(null);
        setProfileError(message);
      }
      return false;
    }
  }, []);
  /** #251: send one playlist or channel URL; the engine queues one job per
   * video with the chosen media spec. Returns true when it was accepted. */
  const sendPlaylist = useCallback(async (url: string, media?: MediaSpec): Promise<boolean> => {
    try {
      const response = await invoke<PlaylistResponse>("submit_playlist", { url, media });
      setAuthPrompt(null);
      setProfileError(null);
      setProfileStatus(playlistQueuedNotice(response));
      return true;
    } catch (error) {
      setAuthPrompt(null);
      setProfileStatus(null);
      setProfileError(formatInvokeError(error));
      return false;
    }
  }, []);
  const addDraft = useCallback(async () => {
    if (submitBlocked || modeUnavailable || submitting) return;
    const inputText = draftRef.current?.value ?? draft;
    const inputs = inputText
      .split(/\r?\n/)
      .map((value) => value.trim())
      .filter(Boolean);
    if (!inputs.length) return;
    setSubmitting(true);
    try {
      if (mediaMode !== "images" && inputs.length === 1 && isPlaylistUrl(inputs[0])) {
        const playlistSpec = mediaSpecForMode(
          mediaMode,
          effectiveVideoQuality,
          mediaKindForInputs(inputText),
          audioQuality,
          pro && withTranscript,
        );
        if (await sendPlaylist(inputs[0], playlistSpec)) {
          setDraft((current) => (current === inputText ? "" : current));
        }
        return;
      }
      if (mediaMode === "images" && inputs.length === 1 && isProfileUrl(inputs[0])) {
        // #248: a profile link batch-saves its newest posts through the engine.
        if (await sendProfile(inputs[0])) {
          setDraft((current) => (current === inputText ? "" : current));
        }
        return;
      }
      if (mediaMode === "images") {
        // Per line: image-capable links save images; the rest transcribe.
        const { image, other } = splitImageLines(inputText);
        const imageOk = image.length ? await onAdd(image, imageSpecForInputs(image.join("\n"))) : true;
        const otherOk = other.length ? await onAdd(other, undefined) : true;
        if (imageOk && otherOk) {
          setDraft((current) => (current === inputText ? "" : current));
        }
        return;
      }
      const spec = mediaSpecForMode(
        mediaMode,
        effectiveVideoQuality,
        mediaKindForInputs(inputText),
        audioQuality,
        pro && withTranscript,
      );
      if (await onAdd(inputs, spec)) {
        setDraft((current) => (current === inputText ? "" : current));
      }
    } finally {
      setSubmitting(false);
    }
  }, [
    audioQuality,
    draft,
    effectiveVideoQuality,
    mediaMode,
    modeUnavailable,
    onAdd,
    pro,
    sendPlaylist,
    sendProfile,
    submitBlocked,
    submitting,
    withTranscript,
  ]);
  const imageFallback =
    mediaMode === "images" && splitImageLines(draft).other.length > 0;
  return (
    <div className="entry">
      <div className="addr" data-submit-blocked={submitBlocked ? "true" : "false"}>
        <Link2 className="glyph" size={15} aria-hidden="true" />
        <textarea
          ref={draftRef}
          rows={1}
          value={draft}
          disabled={submitBlocked}
          aria-label="Paste links, file paths, or screenshots"
          placeholder="Paste links or drop files"
          onChange={(event) => setDraft(event.currentTarget.value)}
          onFocus={() => setFocused(true)}
          onBlur={() => setFocused(false)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && !event.shiftKey) {
              event.preventDefault();
              void addDraft();
            }
          }}
        />
        <button
          type="button"
          className="files"
          disabled={submitBlocked || submitting || modeUnavailable}
          aria-label={
            draft.trim() ? "Add pasted sources to queue" : "Choose image, PDF, or media files"
          }
          title={draft.trim() ? "Add to queue (Enter)" : "Choose Files"}
          onClick={() => (draft.trim() ? void addDraft() : onPickFiles())}
        >
          <Plus size={14} />
        </button>
        {focused && draft.trim() && (
          <div className="suggest show">
            {help ? (
              <p className="sg-head" role="alert">
                {help}
              </p>
            ) : (
              <button
                type="button"
                className="sg"
                disabled={submitBlocked || submitting}
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => void addDraft()}
              >
                <Link2 size={16} />
                <span className="tx">
                  <b>
                    Add{" "}
                    {draft.split(/\r?\n/).filter((line) => line.trim()).length > 1
                      ? "links"
                      : "link"}
                  </b>
                  <small>{draft}</small>
                </span>
                <span className="k">Enter ↵</span>
              </button>
            )}
            {!help && <p className="sg-hint">Shift + Enter adds another link</p>}
          </div>
        )}
      </div>
      <div className="outseg" {...outputSegments.groupProps} data-media-mode={mediaMode}>
        {OUTPUT_MODES.map(({ value, label, Icon }) => (
          <button
            key={value}
            type="button"
            {...outputSegments.getOptionProps(value)}
            aria-label={label}
            disabled={submitBlocked || unavailable(value)}
            title={unavailable(value) ? "This source cannot provide this download format" : label}
          >
            <Icon className="ic" size={14} />
            <span>{label}</span>
          </button>
        ))}
      </div>
      {mediaMode === "audio" && (
        <select
          className="qbtn"
          aria-label="Audio quality"
          disabled={submitBlocked}
          value={audioQuality}
          onChange={(event) => setAudioQuality(event.currentTarget.value as AudioQuality)}
        >
          <option value="best">Best quality</option>
          <option value="compact">Smaller file</option>
        </select>
      )}
      {mediaMode === "video" && (
        <select
          className="qbtn"
          aria-label="Video quality"
          disabled={submitBlocked}
          value={effectiveVideoQuality}
          onChange={(event) => setVideoQuality(event.currentTarget.value as VideoQuality)}
        >
          <option value="p720">720p</option>
          <option value="p1080">1080p</option>
          <option value="p2160" disabled={!pro}>
            4K{pro ? "" : " (Pro)"}
          </option>
          <option value="source" disabled={!pro}>
            Best available{pro ? "" : " (Pro)"}
          </option>
        </select>
      )}
      {(mediaMode === "audio" || mediaMode === "video") && (
        <label
          className="xpill"
          data-checked={pro && withTranscript}
          data-disabled={submitBlocked}
          title={
            pro
              ? "Transcribe while downloading"
              : `Transcript + download needs ${hostAppName()} Pro`
          }
        >
          <input
            type="checkbox"
            className="xpill-input"
            checked={pro && withTranscript}
            disabled={submitBlocked}
            aria-label="+ Transcript"
            onChange={() => {
              if (pro) {
                setWithTranscript((on) => !on);
              } else {
                onOpenPro?.();
              }
            }}
          />
          <span className="xpill-check" aria-hidden="true">
            <Check size={10} strokeWidth={3} />
          </span>
          <span>Transcript</span>
          {!pro && <span className="xpill-pro">Pro</span>}
        </label>
      )}
      {profileStatus && (
        <span className="intake-error" role="status">
          {profileStatus}
        </span>
      )}
      {profileError && (
        <span className="intake-error" role="alert">
          {profileError}
        </span>
      )}
      {authPrompt && (
        <span className="intake-error" role="alert">
          Sign in to {authPrompt.platform} to save this profile&rsquo;s images.{" "}
          <button
            type="button"
            onClick={() => {
              void invoke("sign_in", {
                platform: authPrompt.platform.toLowerCase(),
                url: authPrompt.url,
              }).catch((error) => setProfileError(formatInvokeError(error)));
            }}
          >
            Sign in
          </button>{" "}
          <button
            type="button"
            onClick={() => {
              const { url } = authPrompt;
              void sendProfile(url).then((ok) => {
                if (ok) setDraft((current) => (current.trim() === url ? "" : current));
              });
            }}
          >
            I&rsquo;ve signed in
          </button>
        </span>
      )}
      {imageFallback && (
        <span className="intake-error" role="status">
          Images not available for this link &mdash; transcribing
        </span>
      )}
      {help && !focused && (
        <span className="intake-error" role="alert">
          {help}
        </span>
      )}
    </div>
  );
}

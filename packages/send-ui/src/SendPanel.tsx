import { useState } from "react";
import type { ReactNode } from "react";
import type { SendClient } from "./client.js";
import { isSendError } from "./client.js";
import type { Incoming, SendDevice, Transfer, TrustEntry, TrustState } from "./types.js";
import { useSend } from "./useSend.js";
import type { UseSendOptions } from "./useSend.js";

/** Every string the panel shows. A host passes any it wants in its own words or language. */
export interface SendWords {
  nearby: string;
  nobodyNearby: string;
  scanning: string;
  refresh: string;
  sendFiles: string;
  sendText: string;
  textPlaceholder: string;
  incoming: string;
  accept: string;
  decline: string;
  unverified: string;
  message: string;
  files: (count: number) => string;
  transfers: string;
  noTransfers: string;
  cancel: string;
  dismiss: string;
  trust: string;
  newDevices: string;
  allow: string;
  ask: string;
  deny: string;
  forget: string;
  labelPlaceholder: string;
  saveLabel: string;
  localNetworkBlocked: string;
  states: Record<Transfer["state"], string>;
  /** Words for a typed error. Default: the error code with underscores as spaces. */
  error: (failure: unknown) => string;
}

export const defaultWords: SendWords = {
  nearby: "Nearby devices",
  nobodyNearby: "No devices found yet.",
  scanning: "Looking for devices...",
  refresh: "Look again",
  sendFiles: "Send files...",
  sendText: "Send text",
  textPlaceholder: "Text to send",
  incoming: "Incoming",
  accept: "Accept",
  decline: "Decline",
  unverified: "This sender could not prove who it is.",
  message: "A text message",
  files: (count) => (count === 1 ? "1 file" : `${count} files`),
  transfers: "Transfers",
  noTransfers: "Nothing sent or received yet.",
  cancel: "Cancel",
  dismiss: "Dismiss",
  trust: "Devices",
  newDevices: "New devices",
  allow: "Allow",
  ask: "Ask",
  deny: "Deny",
  forget: "Forget",
  labelPlaceholder: "Name",
  saveLabel: "Save name",
  localNetworkBlocked: "Local network access is blocked for this app.",
  states: {
    waiting: "Waiting",
    active: "Sending",
    done: "Done",
    failed: "Failed",
    cancelled: "Cancelled",
    declined: "Declined",
  },
  error: (failure) =>
    isSendError(failure) ? failure.code.replace(/_/g, " ") : "Something went wrong.",
};

export interface SendPanelProps extends UseSendOptions {
  client: SendClient;
  /**
   * Opens the host's file dialog and resolves absolute paths (an empty list when cancelled).
   * Without it the "Send files" button is not shown.
   */
  pickPaths?: () => Promise<string[]>;
  words?: Partial<SendWords>;
  className?: string;
}

function size(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
}

function Section({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="rk-send__section">
      <h3 className="rk-send__title">{title}</h3>
      {children}
    </section>
  );
}

function DeviceRow(props: {
  device: SendDevice;
  words: SendWords;
  onFiles?: () => void;
  onText: (text: string) => void;
}) {
  const { device, words, onFiles, onText } = props;
  const [text, setText] = useState("");
  return (
    <li className="rk-send__row">
      <div className="rk-send__name">
        <strong>{device.alias}</strong>
        {device.deviceModel ? <span className="rk-send__muted"> {device.deviceModel}</span> : null}
      </div>
      <div className="rk-send__actions">
        {onFiles ? (
          <button type="button" onClick={onFiles}>
            {words.sendFiles}
          </button>
        ) : null}
        <input
          type="text"
          value={text}
          placeholder={words.textPlaceholder}
          aria-label={`${words.textPlaceholder}: ${device.alias}`}
          onChange={(event) => setText(event.target.value)}
        />
        <button
          type="button"
          disabled={text.length === 0}
          onClick={() => {
            onText(text);
            setText("");
          }}
        >
          {words.sendText}
        </button>
      </div>
    </li>
  );
}

function IncomingCard(props: {
  request: Incoming;
  words: SendWords;
  onAnswer: (accept: boolean) => void;
}) {
  const { request, words, onAnswer } = props;
  return (
    <li className="rk-send__card">
      <div>
        <strong>{request.from}</strong>
        <span className="rk-send__muted">
          {" "}
          {request.isMessage
            ? words.message
            : `${words.files(request.fileCount)}, ${size(request.totalBytes)}`}
        </span>
      </div>
      {request.files.length > 0 ? (
        <ul className="rk-send__files">
          {request.files.map((file) => (
            <li key={file.name}>
              {file.name} <span className="rk-send__muted">{size(file.size)}</span>
            </li>
          ))}
        </ul>
      ) : null}
      {request.verified ? null : <p className="rk-send__warn">{words.unverified}</p>}
      <div className="rk-send__actions">
        <button type="button" onClick={() => onAnswer(true)}>
          {words.accept}
        </button>
        <button type="button" onClick={() => onAnswer(false)}>
          {words.decline}
        </button>
      </div>
    </li>
  );
}

function TransferRow(props: {
  transfer: Transfer;
  words: SendWords;
  onCancel: () => void;
  onDismiss: () => void;
}) {
  const { transfer, words, onCancel, onDismiss } = props;
  const open = transfer.state === "waiting" || transfer.state === "active";
  return (
    <li className="rk-send__row">
      <div className="rk-send__name">
        <strong>{transfer.peer}</strong>
        <span className="rk-send__muted">
          {" "}
          {transfer.direction === "send" ? "→" : "←"} {words.states[transfer.state]}
        </span>
        {transfer.error ? <span className="rk-send__warn"> {words.error(transfer.error)}</span> : null}
        {open && transfer.totalBytes > 0 ? (
          <progress value={transfer.doneBytes} max={transfer.totalBytes} />
        ) : null}
        {transfer.message ? <div className="rk-send__message">{transfer.message}</div> : null}
      </div>
      <div className="rk-send__actions">
        {open ? (
          <button type="button" onClick={onCancel}>
            {words.cancel}
          </button>
        ) : (
          <button type="button" onClick={onDismiss}>
            {words.dismiss}
          </button>
        )}
      </div>
    </li>
  );
}

function TrustRow(props: {
  entry: TrustEntry;
  words: SendWords;
  nearby: boolean;
  onState: (state: TrustState) => void;
  onLabel: (label: string) => void;
  onForget: () => void;
}) {
  const { entry, words, nearby, onState, onLabel, onForget } = props;
  const [label, setLabel] = useState(entry.label);
  const shown = entry.label || entry.alias_seen || entry.fingerprint.slice(0, 8);
  return (
    <li className="rk-send__row">
      <div className="rk-send__name">
        <strong>{shown}</strong>
        <input
          type="text"
          value={label}
          placeholder={words.labelPlaceholder}
          aria-label={`${words.labelPlaceholder}: ${shown}`}
          onChange={(event) => setLabel(event.target.value)}
        />
        <button type="button" disabled={label === entry.label} onClick={() => onLabel(label)}>
          {words.saveLabel}
        </button>
      </div>
      <div className="rk-send__actions">
        <select
          value={entry.state}
          aria-label={shown}
          onChange={(event) => onState(event.target.value as TrustState)}
        >
          <option value="allow">{words.allow}</option>
          <option value="ask">{words.ask}</option>
          <option value="deny">{words.deny}</option>
        </select>
        <button type="button" disabled={nearby} onClick={onForget}>
          {words.forget}
        </button>
      </div>
    </li>
  );
}

/** The nearby-sharing screen: devices to send to, requests to answer, transfers and trust. */
export function SendPanel(props: SendPanelProps) {
  const { client, pickPaths, className, subscribe, pollMs } = props;
  const words: SendWords = { ...defaultWords, ...props.words };
  const { snapshot, trust, defaultState, error, run } = useSend(client, { subscribe, pollMs });

  if (!snapshot) return <div className={className ?? "rk-send"} aria-busy="true" />;

  // A failed command is already in `error`; the buttons need no second handler.
  const act = (action: (client: SendClient) => Promise<unknown>) => {
    run(action).catch(() => undefined);
  };
  const nearbyPrints = new Set(snapshot.devices.map((d) => d.fingerprint));

  return (
    <div className={className ?? "rk-send"}>
      {error ? (
        <p className="rk-send__warn" role="alert">
          {words.error(error)}
        </p>
      ) : null}
      {snapshot.localNetwork === "blocked" ? (
        <p className="rk-send__warn" role="alert">
          {words.localNetworkBlocked}
        </p>
      ) : null}

      {snapshot.incoming.length > 0 ? (
        <Section title={words.incoming}>
          <ul className="rk-send__list">
            {snapshot.incoming.map((request) => (
              <IncomingCard
                key={request.id}
                request={request}
                words={words}
                onAnswer={(accept) => act((c) => c.respond(request.id, accept))}
              />
            ))}
          </ul>
        </Section>
      ) : null}

      <Section title={words.nearby}>
        <div className="rk-send__actions">
          <button type="button" onClick={() => act((c) => c.refresh())}>
            {words.refresh}
          </button>
          {snapshot.scanning ? <span className="rk-send__muted">{words.scanning}</span> : null}
        </div>
        {snapshot.devices.length === 0 ? (
          <p className="rk-send__muted">{words.nobodyNearby}</p>
        ) : (
          <ul className="rk-send__list">
            {snapshot.devices.map((device) => (
              <DeviceRow
                key={device.fingerprint}
                device={device}
                words={words}
                onFiles={
                  pickPaths
                    ? () =>
                        act(async (c) => {
                          const paths = await pickPaths();
                          if (paths.length > 0) await c.send({ target: device.fingerprint, paths });
                        })
                    : undefined
                }
                onText={(text) => act((c) => c.send({ target: device.fingerprint, texts: [text] }))}
              />
            ))}
          </ul>
        )}
      </Section>

      <Section title={words.transfers}>
        {snapshot.transfers.length === 0 ? (
          <p className="rk-send__muted">{words.noTransfers}</p>
        ) : (
          <ul className="rk-send__list">
            {[...snapshot.transfers].reverse().map((transfer) => (
              <TransferRow
                key={transfer.id}
                transfer={transfer}
                words={words}
                onCancel={() => act((c) => c.cancel(transfer.id))}
                onDismiss={() => act((c) => c.dismiss(transfer.id))}
              />
            ))}
          </ul>
        )}
      </Section>

      <Section title={words.trust}>
        <label className="rk-send__actions">
          <span>{words.newDevices}</span>
          <select
            value={defaultState === "deny" ? "deny" : "ask"}
            onChange={(event) =>
              act((c) => c.trustSetDefault(event.target.value === "deny" ? "deny" : "ask"))
            }
          >
            <option value="ask">{words.ask}</option>
            <option value="deny">{words.deny}</option>
          </select>
        </label>
        <ul className="rk-send__list">
          {trust.map((entry) => (
            <TrustRow
              key={entry.fingerprint + entry.label + entry.state}
              entry={entry}
              words={words}
              nearby={nearbyPrints.has(entry.fingerprint)}
              onState={(state) => act((c) => c.trustSet(entry.fingerprint, state))}
              onLabel={(label) => act((c) => c.trustLabel(entry.fingerprint, label))}
              onForget={() => act((c) => c.trustForget(entry.fingerprint))}
            />
          ))}
        </ul>
      </Section>
    </div>
  );
}

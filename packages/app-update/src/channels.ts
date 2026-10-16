export type UpdateChannelName = string;

export interface UpdateChannelConfig {
  /** Updater endpoint(s) for this channel. Each must be an https URL (http only for loopback). */
  endpoints: readonly string[];
}

export type UpdateChannelMap = Readonly<Record<UpdateChannelName, UpdateChannelConfig>>;

const NAME = /^[a-z][a-z0-9-]{0,31}$/;

function isLoopback(host: string): boolean {
  return host === 'localhost' || host === '127.0.0.1' || host === '[::1]';
}

/** Throws unless the channel map is well formed: valid names, at least one https endpoint each. */
export function validateChannels(channels: UpdateChannelMap): void {
  const names = Object.keys(channels);
  if (names.length === 0) throw new Error('update channels: at least one channel is required');
  for (const name of names) {
    if (!NAME.test(name)) throw new Error(`update channels: invalid channel name "${name}"`);
    const config = channels[name]!;
    if (!Array.isArray(config.endpoints) || config.endpoints.length === 0) {
      throw new Error(`update channels: channel "${name}" needs at least one endpoint`);
    }
    for (const endpoint of config.endpoints) {
      let url: URL;
      try {
        url = new URL(endpoint);
      } catch {
        throw new Error(`update channels: channel "${name}" has an invalid endpoint`);
      }
      const secure = url.protocol === 'https:' || (url.protocol === 'http:' && isLoopback(url.host.replace(/:\d+$/, '')));
      if (!secure) throw new Error(`update channels: channel "${name}" endpoint must use https`);
    }
  }
}

/**
 * Resolve a channel to its endpoints. An unknown requested channel falls back to
 * `defaultChannel` only when `allowFallback` is true; otherwise it throws.
 */
export function resolveChannel(
  channels: UpdateChannelMap,
  requested: string | null | undefined,
  options: { defaultChannel: string; allowFallback?: boolean },
): { channel: string; endpoints: readonly string[] } {
  validateChannels(channels);
  if (!(options.defaultChannel in channels)) {
    throw new Error(`update channels: default channel "${options.defaultChannel}" is not defined`);
  }
  const name = requested ?? options.defaultChannel;
  if (name in channels) return { channel: name, endpoints: channels[name]!.endpoints };
  if (options.allowFallback) {
    return { channel: options.defaultChannel, endpoints: channels[options.defaultChannel]!.endpoints };
  }
  throw new Error(`update channels: unknown channel "${name}"`);
}

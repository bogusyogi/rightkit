import type { UpdateHeaders } from './index.js';

/** Activation-authenticated manifest request headers: the bearer activation and the app and device the server checks. */
export interface ActivationUpdateHeaderOptions {
  /** Sent as `Authorization: Bearer <activationToken>`. It is never echoed in an error message. */
  activationToken: string;
  /** Header prefix: the names are `X-<headerPrefix>-App` and `X-<headerPrefix>-Device` (for example `MailRight`). */
  headerPrefix: string;
  /** Value of `X-<headerPrefix>-App`, for example the Right Suite app key. */
  app: string;
  /** Value of `X-<headerPrefix>-Device`, the device identifier the activation was issued to. */
  device: string;
}

const PREFIX = /^[A-Za-z][A-Za-z0-9]{0,63}$/u;
// RFC 6750 token68 characters, so the token cannot carry a separator, space or line break into the header.
const TOKEN68 = /^[A-Za-z0-9._~+/=-]{1,4096}$/u;
// Visible ASCII without spaces: no control characters, so no header injection.
const VISIBLE = /^[\x21-\x7e]{1,256}$/u;

/**
 * Builds the headers an app returns from its manifest-check `getHeaders` callback. Throws a TypeError naming the
 * invalid field (never its value) when the prefix, token, app or device is not a safe header value.
 */
export function buildActivationUpdateHeaders(options: ActivationUpdateHeaderOptions): UpdateHeaders {
  if (!PREFIX.test(options.headerPrefix)) {
    throw new TypeError('activation update headers: headerPrefix must start with a letter and hold only letters and digits');
  }
  if (!TOKEN68.test(options.activationToken)) {
    throw new TypeError('activation update headers: activationToken is not a valid bearer token');
  }
  if (!VISIBLE.test(options.app)) throw new TypeError('activation update headers: app is not a valid header value');
  if (!VISIBLE.test(options.device)) throw new TypeError('activation update headers: device is not a valid header value');
  return {
    Authorization: `Bearer ${options.activationToken}`,
    [`X-${options.headerPrefix}-App`]: options.app,
    [`X-${options.headerPrefix}-Device`]: options.device,
  };
}

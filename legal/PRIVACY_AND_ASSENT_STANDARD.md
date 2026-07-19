# Right Suite Privacy and Assent Standard

**Version 1.0 · Effective July 16, 2026**

This internal standard defines where Right Suite legal documents appear and what evidence is retained. It is not a customer privacy notice.

## Website

Every product site must expose stable, public HTTPS routes for Terms, Privacy, Refunds, EULA, Acceptable Use, Product Schedule, and Third-Party Notices. Checkout links must open the exact versions being accepted. The footer may share suite-wide documents, but product-specific schedules, privacy facts, contacts, and notices must resolve on the product's canonical domain.

Before a licensed order or PaymentIntent is created, checkout must show an unchecked control beside conspicuous links and an unambiguous statement that the purchaser:

- agrees to the EULA, Ownership Notice, Acceptable Use Policy, Product Schedule, Terms, and Enterprise Order Terms if applicable;
- confirms authority and commercial-license scope; and
- acknowledges the Privacy Notice and Refund Policy.

“Acknowledges” is not consent. Marketing, telemetry, personalized advertising, special-category data, digital-content withdrawal waiver, or another optional consent requires its own unticked control and may not be a condition unless genuinely necessary.

The server records its timestamp, order/customer reference, app key, surface, acceptance version, document IDs/versions/SHA-256 hashes, and purchaser authority statement. Do not collect an IP address or device fingerprint solely for legal evidence unless a documented necessity and retention rule justifies it.

## Desktop app

Every app must block product use until first-run acceptance succeeds. It must:

- start with no preselected agreement or eligibility choice;
- link the exact bundled and public documents;
- require agreement to the incorporated contracts;
- require an eligibility basis: individual use, or enterprise use under a valid Enterprise Order;
- describe Privacy as a notice and keep optional network/telemetry consent separate;
- persist a local receipt before enabling native hotkeys, background runtime, model use, or other product operation; and
- re-prompt when a material acceptance version changes.

The local receipt contains no name, email, IP address, audio, transcript, or device fingerprint. It is product state, not a security boundary. Clearing app data causes reacceptance.

## Installer

Windows installers should present the bundled EULA when the installer supports clickthrough. Passive/silent enterprise deployment may suppress installer UI, so the app's first-run gate remains authoritative. A macOS DMG may display or bundle the documents but must not be represented as obtaining clickthrough assent.

People do not need to accept website Terms merely to download a free installer. They accept the software contract before first use and purchase terms before payment.

## Versioning

Change `acceptanceVersion` when the grant, eligibility-basis/Seat definition, restrictions, paid scope, termination, warranty/liability, dispute terms, incorporated AUP, or Product Schedule permission changes. Do not reuse a version for different material hashes.

A correction to contact details, formatting, Third-Party Notices, or a Privacy Notice does not itself require EULA reacceptance unless it changes the contractual bargain or an optional consent. Notify users as applicable.

## Retention and access

Order acceptance evidence follows the order/financial-record retention schedule and must be access-controlled. Local app receipts stay until app data is cleared. Legal evidence must not be reused for marketing or profiling.

<!-- DRAFT: pending Adrian review -->
# CutRight Product Schedule

**Schedule version [TBD] · Effective [TBD]**  
**EULA acceptance version:** `[TBD: cutright-YYYY-MM-DD-vN]`  
**Product:** CutRight for [TBD: platforms]  
**Licensor:** Damned Ventures LLC

This Product Schedule forms part of the Right Suite End User License Agreement. Capitalized terms have the meanings in the EULA.

## 1. Product

CutRight is a local-first desktop media editing application. Proprietary Software includes the CutRight user interface, native shell, product assets, and Licensor-created integrations, excluding Third-Party Components identified in the Third-Party Notices.

## 2. Eligibility: individual and enterprise use

- [TBD: edition model. HeardRight wording ("An individual may use HeardRight's Free features at no charge ...") or ViewRight wording ("An individual may use every ViewRight feature at no charge ...") depending on CutRight's edition model.]
- An individual may buy [TBD: paid edition name] for their own use on their own or personally controlled devices.
- Enterprise use — any use by or for an Organization, use an Organization pays for or directs, or use on devices an Organization owns, issues, or manages — requires an Enterprise Order with a Seat for each Worker who uses CutRight, before that use begins.
- Which basis applies is governed by the Individual use and Enterprise use rules in the EULA.

The optional [TBD: paid edition name] purchase is an individual entitlement; it does not authorize Enterprise use. Buying [TBD: paid edition name] on an Organization's behalf does not create an Organization license — Enterprise use needs an Enterprise Order.

## 3. Users and devices

**Free scope:** an individual may install on devices they own or personally control. [TBD: device cap / feature scope statement.]

**[TBD: paid edition scope heading]:** [TBD: per-person or per-device scope of a paid purchase.] It may not be shared among multiple people or used for Enterprise use.

**Enterprise scope:** the accepted Enterprise Order states the number of Seats, permitted Workers, devices, Affiliates, reassignment, and deployment. An order code, paid receipt/invoice, accepted legal-document record, and any signed order form together prove the purchased permission.

## 4. Editions and pricing

[TBD: edition contents and public reference pricing as of this schedule's effective date.]

The exact price, currency, taxes, edition, and refund terms shown at checkout control that order. Enterprise (per-Seat) pricing requires an Enterprise Order and is not created by purchasing an individual key without written authorization.

[TBD: subscription / automatic-renewal statement.]

## 5. License duration and updates

Free permission continues while you remain eligible and comply with the EULA. [TBD: duration of a paid license and which updates it includes.]

[TBD: if "lifetime updates" is offered, insert the template Product Lifetime definition: "Product Lifetime means the period during which Damned Ventures LLC continues to develop and distribute CutRight as a supported product. It does not mean the purchaser's lifetime, guarantee a minimum support period, require creation of future features, or require continued operation of a third-party service. The same definition must appear wherever “lifetime updates” is marketed."]

Signed security and compatibility patches remain available to Free and [TBD: paid edition name] users when published. Feature updates may be entitlement-gated under the public Right Suite update manifest, but packaging, signing, and security do not vary by tier.

## 6. Local data and storage

Editing runs on the device. Source media and projects stay on the device, and CutRight does not upload footage to Damned Ventures LLC. Exports and renders are written locally.

Downloaded models are stored in the shared Orthic Labs model store on the device. A per-install random device identifier used for model downloads is stored locally. An Anthropic API key, if the user adds one, is stored in the operating-system keychain. Users are responsible for device access, backups, retention, and content entered into other applications.

## 7. Network features

CutRight may contact network services for:

- paid license activation, refresh, deactivation, trial, and revocation status using app key/version, license or trial key, and a pseudonymous device identifier;
- signed application update checks using product, version, platform, and ordinary connection metadata;
- speech-model (Parakeet) downloads from the Orthic model service at heardright-models.adrdsouza.workers.dev, gated by the per-install random device identifier, and face-detection-model (YuNet) downloads from GitHub (opencv_zoo), using ordinary connection metadata;
- online checkout, receipts, refunds, support, and commercial-order administration; and
- optional AI moment scoring when the user adds their own Anthropic API key.

CutRight has no analytics, no telemetry, and no user account.

CutRight may use the HeardRight speech service running on the same device through a local socket restricted to the current user. Nothing leaves the device for this.

## 8. AI and automated processing

Moment scoring for shorts uses a local heuristic unless the user adds their own Anthropic API key. When a key is added, the app sends transcript text directly from the device to Anthropic (api.anthropic.com). Media is not sent for moment scoring. Provider availability, accuracy, charges, retention, and training controls are governed by the user's provider account and terms.

Users must not send content to a provider unless they have authority to do so and must configure any provider-side privacy, retention, region, and no-training controls they require.

Generated image, video, voice, avatar, and music assets come through the separately installed GenRight app and its providers. CutRight only imports the files GenRight returns. GenRight's own terms govern its providers.

## 9. Commands and high-impact use

Users must review material output before relying on it. CutRight must not be the sole control for a high-impact or safety-critical use described in the AUP.

## 10. Support and refunds

Support is provided on a reasonable-efforts basis at [TBD: support email] unless a Commercial Order states a different service level. [TBD: "No response or resolution time is guaranteed by the public one-time license." if the paid offer is one-time.]

[TBD: refund window or Refund Policy reference.] Mandatory consumer rights continue to apply. Refunds revoke the affected paid key; they do not create Enterprise permission for an Organization.

## 11. Public documents and contact

- Terms: <https://orthiclabs.com/cutright/terms/>
- Privacy Notice: <https://orthiclabs.com/cutright/legal/privacy/>
- Refund Policy: <https://orthiclabs.com/cutright/refund/>
- EULA: <https://orthiclabs.com/cutright/legal/eula/>
- Ownership Notice: <https://orthiclabs.com/cutright/legal/license/>
- Acceptable Use: <https://orthiclabs.com/cutright/legal/acceptable-use/>
- Product Schedule: <https://orthiclabs.com/cutright/legal/product-schedule/>
- Third-Party Notices: <https://orthiclabs.com/cutright/legal/third-party/>
- Pricing: <https://orthiclabs.com/cutright/pricing/>

Support and privacy requests: [TBD: support email]  
Legal requests: legal@damnedventures.com  
Damned Ventures LLC, 169 Madison Ave, New York, NY 10016, United States  
+1 337-602-9228

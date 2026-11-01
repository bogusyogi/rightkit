use super::ApiEndpoint;

// The /memright/* routes were removed with the Membrane CLI cutover (#26): memory and recall
// now live in Pulse's store behind the `membrane` CLI. The group stays (empty) so the
// manifest's group order and offsets are unchanged.
pub(super) const ENDPOINT_GROUP_09: [ApiEndpoint; 0] = [];

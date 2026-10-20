//! Several caps under one request id, and the epoch-reset device allowance.
//!
//! [`Ledger::reserve_multi`] holds one estimate against every [`Cap`] of a list, all or nothing:
//! it runs in one transaction (or savepoint), so a cap that refuses leaves no row, no total and
//! no budget tag on any other cap. Each cap gets its own reservation row (request id
//! `<request_id>#<leg>`), so the existing per-reservation life, reports and acknowledgements
//! apply to every leg; the group record ties the legs to the one request id for idempotent replay
//! and for settling them together.

use rightkit_db::rusqlite::types::Value;

use super::*;

/// One cap a [`Ledger::reserve_multi`] must pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cap {
    scope: String,
    period: String,
    carry_over: bool,
    /// Set for a device cap: the period is the device's current epoch period, resolved when the
    /// reservation is made.
    device: Option<String>,
}

impl Cap {
    /// The allowance of `(scope, period)`.
    pub fn allowance(scope: impl Into<String>, period: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            period: period.into(),
            carry_over: false,
            device: None,
        }
    }

    /// The device allowance of `device_id` in `scope` ([`Ledger::set_device_allowance`]), in its
    /// current epoch. A device with no allowance set (or cleared) imposes no cap and takes no
    /// leg.
    pub fn device(scope: impl Into<String>, device_id: impl Into<String>) -> Self {
        Self {
            scope: scope.into(),
            period: String::new(),
            carry_over: false,
            device: Some(device_id.into()),
        }
    }

    /// Count open reservations of earlier periods against this cap ([`ReserveOptions::carry_over`]).
    /// Ignored for a device cap, whose epochs are reset on purpose.
    pub fn carry_over(mut self, carry_over: bool) -> Self {
        self.carry_over = carry_over;
        self
    }

    fn key(&self) -> String {
        match &self.device {
            Some(device) => format!("d\u{1f}{}\u{1f}{device}", self.scope),
            None => format!(
                "a\u{1f}{}\u{1f}{}\u{1f}{}",
                self.scope, self.period, self.carry_over
            ),
        }
    }
}

/// The legs of a multi-cap reservation, in the order of the caps that applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupOutcome {
    pub request_id: String,
    /// One outcome per leg; each carries its own allowance as it is after the call.
    pub legs: Vec<Outcome>,
    /// True when the call found the work already done and wrote nothing.
    pub replayed: bool,
}

/// A device allowance: the most one device may hold in the current epoch. Every set or clear
/// starts a new epoch, so spend made before the reset no longer counts against it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceAllowance {
    pub scope: String,
    pub device_id: String,
    /// Starts at 1; bumped by every set or clear.
    pub epoch: i64,
    /// `None` when cleared.
    pub limit_micro: Option<i64>,
    /// Unix seconds of the last set or clear.
    pub set_at: i64,
    /// The allowance period of the current epoch (`device:<id>:<epoch>`), when a limit is set.
    pub period: Option<String>,
    /// The current epoch's allowance with its totals, when a limit is set.
    pub allowance: Option<Allowance>,
}

fn device_period(device_id: &str, epoch: i64) -> String {
    format!("device:{device_id}:{epoch}")
}

struct DeviceRow {
    epoch: i64,
    limit_micro: Option<i64>,
    set_at: i64,
}

fn device_row(conn: &Connection, scope: &str, device_id: &str) -> Result<Option<DeviceRow>> {
    Ok(conn
        .query_row(
            "SELECT epoch, limit_micro, set_at FROM spend_device_allowances \
             WHERE scope = ?1 AND device_id = ?2",
            params![scope, device_id],
            |row| {
                let raw: Option<Value> = row.get(1)?;
                Ok(DeviceRow {
                    epoch: row.get(0)?,
                    limit_micro: match raw {
                        None | Some(Value::Null) => None,
                        Some(v) => Some(limit_from_raw(v)),
                    },
                    set_at: row.get(2)?,
                })
            },
        )
        .optional()?)
}

fn device_allowance_impl(
    conn: &Connection,
    scope: &str,
    device_id: &str,
) -> Result<Option<DeviceAllowance>> {
    let Some(row) = device_row(conn, scope, device_id)? else {
        return Ok(None);
    };
    let period = row.limit_micro.map(|_| device_period(device_id, row.epoch));
    let allowance = match &period {
        Some(p) => get_allowance(conn, scope, p)?,
        None => None,
    };
    Ok(Some(DeviceAllowance {
        scope: scope.to_owned(),
        device_id: device_id.to_owned(),
        epoch: row.epoch,
        limit_micro: row.limit_micro,
        set_at: row.set_at,
        period,
        allowance,
    }))
}

fn set_device_allowance_impl(
    conn: &Connection,
    ctx: &Ctx,
    scope: &str,
    device_id: &str,
    limit_micro: Option<i64>,
    note: Option<&str>,
) -> Result<DeviceAllowance> {
    non_empty("scope", scope)?;
    non_empty("device_id", device_id)?;
    if limit_micro.is_some_and(|l| l < 0) {
        return Err(SpendError::BadInput("limit_micro is negative".into()));
    }
    let note = note.map(str::trim).filter(|n| !n.is_empty());
    if note.is_some_and(|n| n.chars().count() > 500) {
        return Err(SpendError::BadInput(
            "note must be at most 500 characters".into(),
        ));
    }
    let prior = device_row(conn, scope, device_id)?;
    let epoch = match &prior {
        Some(p) => p.epoch.checked_add(1).ok_or(SpendError::Overflow)?,
        None => 1,
    };
    conn.execute(
        "INSERT INTO spend_device_allowances (scope, device_id, epoch, limit_micro, set_at) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (scope, device_id) DO UPDATE SET epoch = excluded.epoch, \
         limit_micro = excluded.limit_micro, set_at = excluded.set_at",
        params![scope, device_id, epoch, limit_micro, ctx.now],
    )?;
    let period = device_period(device_id, epoch);
    if let Some(limit) = limit_micro {
        conn.execute(
            "INSERT INTO spend_allowances (scope, period, limit_micro, reserved_micro, \
             committed_micro, revision, updated_at) VALUES (?1, ?2, ?3, 0, 0, 1, ?4)",
            params![scope, period, limit, ctx.now],
        )?;
    }
    audit::record(
        conn,
        ctx,
        "device_allowance",
        scope,
        &period,
        (prior.as_ref().and_then(|p| p.limit_micro), limit_micro),
        (prior.as_ref().map(|p| p.epoch), Some(epoch)),
        note,
    )?;
    device_allowance_impl(conn, scope, device_id)?
        .ok_or_else(|| SpendError::NotFound(device_id.to_owned()))
}

struct GroupRow {
    route_id: String,
    price_version: String,
    estimate_micro: i64,
    account: Option<String>,
    currency: Option<String>,
    caps: String,
}

fn find_group(conn: &Connection, request_id: &str) -> Result<Option<GroupRow>> {
    Ok(conn
        .query_row(
            "SELECT route_id, price_version, estimated_micro, account, currency, caps \
             FROM spend_groups WHERE request_id = ?1",
            [request_id],
            |row| {
                Ok(GroupRow {
                    route_id: row.get(0)?,
                    price_version: row.get(1)?,
                    estimate_micro: row.get(2)?,
                    account: row.get(3)?,
                    currency: row.get(4)?,
                    caps: row.get(5)?,
                })
            },
        )
        .optional()?)
}

/// The legs of a group in leg order, or `NotFound` when there is no such group.
fn group_legs(conn: &Connection, request_id: &str) -> Result<Vec<Reservation>> {
    let seqs: Vec<i64> = {
        let mut stmt =
            conn.prepare("SELECT seq FROM spend_group_legs WHERE request_id = ?1 ORDER BY leg")?;
        let rows = stmt.query_map([request_id], |row| row.get(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    if seqs.is_empty() {
        return Err(SpendError::NotFound(request_id.to_owned()));
    }
    seqs.into_iter()
        .map(|seq| require_reservation(conn, &format!("res-{seq:08}")))
        .collect()
}

fn group_outcome(conn: &Connection, request_id: &str, replayed: bool) -> Result<GroupOutcome> {
    let mut legs = Vec::new();
    for res in group_legs(conn, request_id)? {
        legs.push(outcome(conn, res, replayed)?);
    }
    Ok(GroupOutcome {
        request_id: request_id.to_owned(),
        legs,
        replayed,
    })
}

/// A refusal that belongs to one cap is named with the cap; storage errors pass through.
fn name_cap(err: SpendError, scope: &str, period: &str) -> SpendError {
    match err {
        SpendError::AllowanceMissing { .. }
        | SpendError::AllowanceExceeded { .. }
        | SpendError::OverrunUnacknowledged { .. } => SpendError::CapRefused {
            scope: scope.to_owned(),
            period: period.to_owned(),
            cause: Box::new(err),
        },
        other => other,
    }
}

#[allow(clippy::too_many_arguments)]
fn reserve_multi_impl(
    conn: &Connection,
    ctx: &Ctx,
    request_id: &str,
    caps: &[Cap],
    route_id: &str,
    price_version: &str,
    estimate_micro: i64,
    options: &ReserveOptions,
) -> Result<GroupOutcome> {
    non_empty("request_id", request_id)?;
    non_empty("route_id", route_id)?;
    non_empty("price_version", price_version)?;
    options.validate()?;
    if estimate_micro < 0 {
        return Err(SpendError::BadInput("estimate_micro is negative".into()));
    }
    if caps.is_empty() {
        return Err(SpendError::BadInput("no caps given".into()));
    }
    for cap in caps {
        non_empty("scope", &cap.scope)?;
        match &cap.device {
            Some(device) => non_empty("device_id", device)?,
            None => non_empty("period", &cap.period)?,
        }
    }
    let mut keys: Vec<String> = caps.iter().map(Cap::key).collect();
    keys.sort();
    let fingerprint = keys.join("\u{1e}");

    if let Some(stored) = find_group(conn, request_id)? {
        let same = stored.route_id == route_id
            && stored.price_version == price_version
            && stored.estimate_micro == estimate_micro
            && stored.account == options.account
            && stored.currency == options.currency
            && stored.caps == fingerprint;
        if !same {
            return Err(SpendError::RequestMismatch {
                request_id: request_id.to_owned(),
            });
        }
        // The original first leg owns the budget tags. Missing leg zero is not an
        // untagged request, even if later legs survive; never use the current caps order.
        let first_seq: i64 = conn
            .query_row(
                "SELECT seq FROM spend_group_legs WHERE request_id = ?1 AND leg = 0",
                [request_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| SpendError::NotFound(request_id.to_owned()))?;
        let first = require_reservation(conn, &format!("res-{first_seq:08}"))?;
        budget::check_replay_budget(conn, options, &first, request_id)?;
        return group_outcome(conn, request_id, true);
    }

    // Resolve the caps that apply: a device cap with no allowance set imposes none.
    let mut resolved: Vec<(&Cap, String)> = Vec::new();
    for cap in caps {
        let period = match &cap.device {
            None => cap.period.clone(),
            Some(device) => match device_row(conn, &cap.scope, device)? {
                Some(row) if row.limit_micro.is_some() => device_period(device, row.epoch),
                _ => continue,
            },
        };
        if resolved
            .iter()
            .any(|(c, p)| c.scope == cap.scope && *p == period)
        {
            return Err(SpendError::BadInput(format!(
                "cap {:?}/{period:?} is listed twice",
                cap.scope
            )));
        }
        resolved.push((cap, period));
    }
    if resolved.is_empty() {
        return Err(SpendError::BadInput(
            "no cap applies: every device cap has no allowance set".into(),
        ));
    }

    conn.execute(
        "INSERT INTO spend_groups (request_id, route_id, price_version, estimated_micro, \
         account, currency, caps, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            request_id,
            route_id,
            price_version,
            estimate_micro,
            options.account,
            options.currency,
            fingerprint,
            ctx.now
        ],
    )?;
    let mut first: Option<Reservation> = None;
    for (leg, (cap, period)) in resolved.iter().enumerate() {
        let leg_request = format!("{request_id}#{leg}");
        if find_by_request(conn, &leg_request)?.is_some() {
            return Err(SpendError::RequestMismatch {
                request_id: leg_request,
            });
        }
        let carry = cap.carry_over && cap.device.is_none();
        let out = reserve_core(
            conn,
            ctx,
            &cap.scope,
            &leg_request,
            period,
            route_id,
            price_version,
            estimate_micro,
            options,
            carry,
        )
        .map_err(|e| name_cap(e, &cap.scope, period))?;
        conn.execute(
            "INSERT INTO spend_group_legs (request_id, leg, seq) VALUES (?1, ?2, ?3)",
            params![
                request_id,
                leg as i64,
                seq_of(&out.reservation.reservation_id)?
            ],
        )?;
        if first.is_none() {
            first = Some(out.reservation);
        }
    }
    // The budgets count the estimate once per request, so they tag the first leg only.
    if let Some(first) = first {
        budget::apply_budgets(conn, ctx, options, &first)?;
    }
    group_outcome(conn, request_id, false)
}

fn mark_in_flight_group_impl(
    conn: &Connection,
    ctx: &Ctx,
    request_id: &str,
) -> Result<GroupOutcome> {
    settle_group(conn, request_id, |res| {
        mark_in_flight_impl(conn, ctx, &res.reservation_id)
    })
}

fn commit_group_impl(
    conn: &Connection,
    ctx: &Ctx,
    request_id: &str,
    actual_micro: i64,
) -> Result<GroupOutcome> {
    settle_group(conn, request_id, |res| {
        commit_impl(conn, ctx, &res.reservation_id, actual_micro)
    })
}

fn release_group_impl(
    conn: &Connection,
    ctx: &Ctx,
    request_id: &str,
    reason: Option<&str>,
) -> Result<GroupOutcome> {
    settle_group(conn, request_id, |res| {
        release_impl(conn, ctx, &res.reservation_id, reason)
    })
}

fn settle_group(
    conn: &Connection,
    request_id: &str,
    mut step: impl FnMut(&Reservation) -> Result<Outcome>,
) -> Result<GroupOutcome> {
    let mut legs = Vec::new();
    for res in group_legs(conn, request_id)? {
        legs.push(step(&res)?);
    }
    let replayed = legs.iter().all(|o| o.replayed);
    Ok(GroupOutcome {
        request_id: request_id.to_owned(),
        legs,
        replayed,
    })
}

impl Ledger {
    /// Hold `estimate_micro` against every cap in `caps` under one `request_id`, all or
    /// nothing. Each cap is checked as [`Ledger::reserve`] checks one allowance (overrun gate,
    /// remaining, carry-over when the cap asks for it); the first refusal is returned as
    /// [`SpendError::CapRefused`] naming the cap and nothing is written on any cap. A device
    /// cap with no allowance set takes no leg; at least one cap must apply.
    ///
    /// Idempotent on `request_id`: route, price version, estimate, account, currency, optional
    /// normalized caller-budget key, and cap fingerprint must match; otherwise this returns
    /// [`SpendError::RequestMismatch`]. Reordered matching caps return the original stored legs.
    /// Caller limits and the process budget are first-admission policy, checked once and
    /// counted once. Replays neither check them again nor change the original budget tags.
    pub fn reserve_multi(
        &mut self,
        request_id: &str,
        caps: &[Cap],
        route_id: &str,
        price_version: &str,
        estimate_micro: i64,
        options: &ReserveOptions,
    ) -> Result<GroupOutcome> {
        self.run(|conn, ctx| {
            reserve_multi_impl(
                conn,
                ctx,
                request_id,
                caps,
                route_id,
                price_version,
                estimate_micro,
                options,
            )
        })
    }

    /// [`Ledger::reserve_multi`] inside the caller's transaction ([`Ledger::begin`]).
    #[allow(clippy::too_many_arguments)]
    pub fn reserve_multi_in(
        &self,
        tx: &Transaction<'_>,
        request_id: &str,
        caps: &[Cap],
        route_id: &str,
        price_version: &str,
        estimate_micro: i64,
        options: &ReserveOptions,
    ) -> Result<GroupOutcome> {
        self.run_in(tx, |conn, ctx| {
            reserve_multi_impl(
                conn,
                ctx,
                request_id,
                caps,
                route_id,
                price_version,
                estimate_micro,
                options,
            )
        })
    }

    /// [`Ledger::reserve_multi`] on a connection with an open transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn reserve_multi_on(
        &self,
        conn: &Connection,
        request_id: &str,
        caps: &[Cap],
        route_id: &str,
        price_version: &str,
        estimate_micro: i64,
        options: &ReserveOptions,
    ) -> Result<GroupOutcome> {
        self.run_on(conn, |conn, ctx| {
            reserve_multi_impl(
                conn,
                ctx,
                request_id,
                caps,
                route_id,
                price_version,
                estimate_micro,
                options,
            )
        })
    }

    /// The legs of a multi-cap reservation, if the request id made one.
    pub fn group(&self, request_id: &str) -> Result<Option<GroupOutcome>> {
        if find_group(&self.conn, request_id)?.is_none() {
            return Ok(None);
        }
        Ok(Some(group_outcome(&self.conn, request_id, false)?))
    }

    /// [`Ledger::mark_in_flight`] on every leg of the group, atomically.
    pub fn mark_in_flight_group(&mut self, request_id: &str) -> Result<GroupOutcome> {
        self.run(|conn, ctx| mark_in_flight_group_impl(conn, ctx, request_id))
    }

    /// [`Ledger::mark_in_flight_group`] inside the caller's transaction.
    pub fn mark_in_flight_group_in(
        &self,
        tx: &Transaction<'_>,
        request_id: &str,
    ) -> Result<GroupOutcome> {
        self.run_in(tx, |conn, ctx| {
            mark_in_flight_group_impl(conn, ctx, request_id)
        })
    }

    /// [`Ledger::mark_in_flight_group`] on a connection with an open transaction.
    pub fn mark_in_flight_group_on(
        &self,
        conn: &Connection,
        request_id: &str,
    ) -> Result<GroupOutcome> {
        self.run_on(conn, |conn, ctx| {
            mark_in_flight_group_impl(conn, ctx, request_id)
        })
    }

    /// [`Ledger::commit`] every leg of the group at the same actual, atomically. Each cap
    /// records the full actual: the call cost that much against each of them.
    pub fn commit_group(&mut self, request_id: &str, actual_micro: i64) -> Result<GroupOutcome> {
        self.run(|conn, ctx| commit_group_impl(conn, ctx, request_id, actual_micro))
    }

    /// [`Ledger::commit_group`] inside the caller's transaction.
    pub fn commit_group_in(
        &self,
        tx: &Transaction<'_>,
        request_id: &str,
        actual_micro: i64,
    ) -> Result<GroupOutcome> {
        self.run_in(tx, |conn, ctx| {
            commit_group_impl(conn, ctx, request_id, actual_micro)
        })
    }

    /// [`Ledger::commit_group`] on a connection with an open transaction.
    pub fn commit_group_on(
        &self,
        conn: &Connection,
        request_id: &str,
        actual_micro: i64,
    ) -> Result<GroupOutcome> {
        self.run_on(conn, |conn, ctx| {
            commit_group_impl(conn, ctx, request_id, actual_micro)
        })
    }

    /// [`Ledger::release`] every leg of the group, atomically.
    pub fn release_group(
        &mut self,
        request_id: &str,
        reason: Option<&str>,
    ) -> Result<GroupOutcome> {
        self.run(|conn, ctx| release_group_impl(conn, ctx, request_id, reason))
    }

    /// [`Ledger::release_group`] inside the caller's transaction.
    pub fn release_group_in(
        &self,
        tx: &Transaction<'_>,
        request_id: &str,
        reason: Option<&str>,
    ) -> Result<GroupOutcome> {
        self.run_in(tx, |conn, ctx| {
            release_group_impl(conn, ctx, request_id, reason)
        })
    }

    /// [`Ledger::release_group`] on a connection with an open transaction.
    pub fn release_group_on(
        &self,
        conn: &Connection,
        request_id: &str,
        reason: Option<&str>,
    ) -> Result<GroupOutcome> {
        self.run_on(conn, |conn, ctx| {
            release_group_impl(conn, ctx, request_id, reason)
        })
    }

    /// Set (`Some`) or clear (`None`) the device allowance of `device_id` in `scope`. Either way
    /// a new epoch starts: a set creates a fresh allowance for it (`device:<id>:<epoch>`, limit
    /// as given, nothing counted), so spend and open reservations of earlier epochs stop
    /// counting; they stay on their own epoch's allowance and settle there. An audit row
    /// records the old and new limit and epoch.
    pub fn set_device_allowance(
        &mut self,
        scope: &str,
        device_id: &str,
        limit_micro: Option<i64>,
        note: Option<&str>,
    ) -> Result<DeviceAllowance> {
        self.run(|conn, ctx| {
            set_device_allowance_impl(conn, ctx, scope, device_id, limit_micro, note)
        })
    }

    /// [`Ledger::set_device_allowance`] inside the caller's transaction.
    pub fn set_device_allowance_in(
        &self,
        tx: &Transaction<'_>,
        scope: &str,
        device_id: &str,
        limit_micro: Option<i64>,
        note: Option<&str>,
    ) -> Result<DeviceAllowance> {
        self.run_in(tx, |conn, ctx| {
            set_device_allowance_impl(conn, ctx, scope, device_id, limit_micro, note)
        })
    }

    /// [`Ledger::set_device_allowance`] on a connection with an open transaction.
    pub fn set_device_allowance_on(
        &self,
        conn: &Connection,
        scope: &str,
        device_id: &str,
        limit_micro: Option<i64>,
        note: Option<&str>,
    ) -> Result<DeviceAllowance> {
        self.run_on(conn, |conn, ctx| {
            set_device_allowance_impl(conn, ctx, scope, device_id, limit_micro, note)
        })
    }

    /// The device allowance of `device_id` in `scope`, if one was ever set or cleared.
    pub fn device_allowance(
        &self,
        scope: &str,
        device_id: &str,
    ) -> Result<Option<DeviceAllowance>> {
        device_allowance_impl(&self.conn, scope, device_id)
    }
}

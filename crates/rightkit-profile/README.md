# rightkit-profile

The owner's profile for an assistant (RK-8, the logic of the Nucleus family of Membrane). It holds the confirmed picture of the owner as entries in areas, decides which questions are due, builds the card a run receives, and writes and reads a neutral export. Pure logic and plain values: no network, no model calls, no clock reads and no files. The host supplies the time and the storage.

Not published (`publish = false`, 0.1.0, unreleased). Written, not yet compiled. The Nucleus contract (`membrane/docs/contracts/nucleus.md` in Pulse) is a draft that awaits its reviewer; this crate implements only what it states and leaves its open points open.

## The name

`rightkit-profile` is a proposal, and so is "Nucleus" (decision 11: a working name). The name is in `Cargo.toml` (`name`, and `[lib] name` beside it) and the directory; `rightkit_profile::NAME` reads it back and no source file spells it. Until the name is approved the crate is `publish = false`, so it needs no feed row and no consumer recipe.

## Modules

| Module | Job |
|---|---|
| `area` | The ten areas (identity, work, jobs, now, people, health, money, home and travel, routines, writing), their slugs, the sensitivity floor of health and money, and `Area::CARD` |
| `entry` | `Entry` (value, source, `true_from`, state, sensitivity, optional `review_by`, history, the owner's confirmation), the checks on keys, values, references and dates |
| `profile` | `ProfileState` and the only ways it changes: `propose`, `set_by_owner`, `confirm`, `reject`, `mark_stale` |
| `interview` | `questions`: stale entries first, then unanswered template questions, at most a limit |
| `card` | `card`: confirmed entries of the chosen areas that the health and money `Switches` allow |
| `neutral` | `export` and `import`: one JSON line per entry or proposal, format `rightkit.profile.neutral.v1` |

## Rules this crate keeps

1. Only the owner confirms. A repository, a mail or a memory can only `propose`; a proposal naming the owner as its source is refused. The ways into the confirmed entries (`set_by_owner`, `confirm`, `reject`) take an `OwnerConfirmation` the host builds from the owner's own words or an approval the owner answered. Nothing here makes one.
2. A proposal never changes a value. A proposal that disagrees with a confirmed entry turns that entry stale, so it comes back as a question; the proposal waits beside it.
3. A changed value keeps the old one as history (`Superseded`, up to 200). "Jobs I have had" builds from history.
4. A model sees confirmed entries only, and health and money entries only when their switch is on. The area's floor applies whatever label an entry carries, so a health entry labelled ordinary is still switched.
5. Import cannot confirm. An entry line must carry the owner's confirmation record; a proposal line must carry none. Import never overwrites: a line whose id exists with different content is rejected and reported, and the other lines still land.

## Open points left open (Nucleus contract, section 11)

| Point | What this crate does meanwhile |
|---|---|
| The on-disk form and location of the area files (2) | Not built. No file is read or written. The neutral export is a JSON-lines export, not the human-editable file form. A file adapter replaces nothing here; it reads and writes `ProfileState` |
| Sensitivity scale and the review date field (3) | `Sensitivity` is `Ordinary`, `Health`, `Money` and non-exhaustive. `review_by` is an optional stored date; a host that derives it passes the derived date in |
| A `retired` state, rejected proposals (4) | No `retired`; history covers it. A rejected proposal is dropped and can be proposed again |
| Untrusted proposals, confirmation from a paired device (5) | Every proposal is stored. `OwnerConfirmation::reference` names the proof, including a device id, and nothing distinguishes the channel |
| Task kind to areas (6) | The host passes the area list to `card`; only the identity card's areas are named (`Area::CARD`) |
| Switch defaults and scope (7) | `Switches::default()` is both off; the host owns the settings |
| Questions per turn, review periods (8) | `questions` takes a limit; templates and wording are the host's |
| Sync (9) | `Entry` carries `created_at` and `updated_at` (unix milliseconds) and an id, which is what a `rightkit-sync` mapper needs. How an edit orders against "text never changes" is open; here an owner edit of a value replaces it and keeps the old one as history |
| Which file is the health record (10) | Not read. Health entries are adopted when that is decided |

## How a host drives it

```text
mail fact / memory / repository -> state.propose(Proposal{..}, now)            stored beside the entries
interview                       -> state.questions(&templates, limit)          stale first, then unanswered
owner answers                   -> state.set_by_owner(EntryInput{..}, &owner, now)
owner says yes to a proposal    -> state.confirm(&proposal_id, &owner, review_by, now)
daily                           -> state.mark_stale(today, now)                review dates come due
each run                        -> state.card(&Area::CARD, switches).to_text()
layout change                   -> neutral::export(&state), then neutral::import(&mut new_state, &text)
```

## Not verified here

Not compiled, linted or run. No unit tests, per the 2026-11-03 rule; acceptance is the journeys U1, U2 and U4 of the plan on an installed build.

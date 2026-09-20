# Litter relay topology: where the cross-litter message plane goes

Status: OPEN DESIGN for §"Direction" — to be reviewed. The **stopgap relay
is built** (see "What is built" below); the raft-collected, agent-to-agent
end state is not. Companion to `docs/LITTER_STATE_MACHINE.md` and
`docs/LITTER_RAFT_LOOP.md` (which see for states and traffic).

## What is built (stopgap relay, 2026-09-20)

Hub-to-hub, exactly one hop, signed **by agents**. A key is an agent's
identity: a litter is not a signing party, it is the swarm a message came
from. So `sig` names the agent that said something, `rs` names the agent
that carried it, and `ol` names the swarm it started in.

Config keys (`/etc/meow/config`):

| Key | Meaning |
|-----|---------|
| `litter_name` | The swarm this agent belongs to — the envelope's `ol`. **Absent ⇒ relay off.** Not a signing identity. |
| `litter_key` | **This agent's** Ed25519 seed, 64 hex chars — every scope has its own. Generated and saved on first run when absent; keep it stable, since pinning this agent means pinning this seed. |
| `litter_peer_keys` | `name:<64-hex-pubkey>,…` — a **guest list**, not a name binding: any listed key may sign for any name. **Unset ⇒ everyone is accepted** (the default; this is a LAN, not a hostile network). With the list unset, only the signature's *shape* is checked — Ed25519 needs the signer's public key and the envelope carries none, so a stranger's signature is unverifiable by construction, not merely untrusted. Pinning a key later verifies that traffic retroactively. |
| `litter_static_peers` | `name@host:port,…` — who to relay to, and the discovery probe list. |

Standing up the Mac side so a peer can reach in (`litter/yard.sh`): the
hub must listen on more than loopback **and** the port must be published,
both opt-in because the default keeps a litter inside its container.

```bash
HUB_ADDR=0.0.0.0:7700 HUB_PUBLISH=7700:7700 \
  LITTER_STATIC_PEERS=ryzen@<peer-ip>:7700 litter/yard.sh start
```

Mechanics:

- **Sign, at the source.** The sending agent signs in its own process,
  before any hub sees the message (`sig::signed_send`, called from
  `hub::tool_send_message`). `ot` is the **sender's** clock, because the
  signed payload has to contain the timestamp. A hub never mints a
  signature over words it did not say.
- **Capture.** `HubState::handle`'s `Send` arm records every *locally
  originated* chat message in `relay_log` together with the sender's
  envelope, verbatim. An unsigned send is simply not relayable and stays
  litter-local. Relayed-in traffic (`rl` present) returns before this
  point, so a hub is never a transit node — one hop, by construction.
- **Forward.** The raft thread's `relay_jobs`/`relay_send` drain that log
  to each online static peer, adding the relaying **agent's** name (`rl`)
  and its signature (`rs`) beside the untouched `sig`, and advancing
  `StaticPeer::last_relay_ts` **only on a confirmed `Sent`**. That cursor
  is measured in our own hub's clock (`RelayJob::cursor_ts`), never in the
  sender's `ot` — two different clocks, and comparing them would skip or
  re-send traffic.
- **Accept.** The receiving hub verifies `sig` (the sender) and `rs` (the
  carrier) against its guest list — or accepts any well-formed envelope
  when that list is unset — checks `(ol, from, ot)` against the seen-set,
  drops anything whose `ol` is our own litter (our words bouncing back),
  and delivers under the
  **original sender's bare name**: provenance never rewrites who said it.
  Relayed-in traffic returns before the task-table hook, so a peer litter
  can talk to ours but cannot mint work in it; the operator (`root`) role
  is only ever stamped on locally-originated messages.
- **Bound.** `RELAY_LOG_CAP` (256) caps the outbox; `RELAY_MAX_AGE_US`
  (60s) discards stale traffic and doubles as the seen-table's memory
  (`prune_seen`), with `SEEN_CAP` as a flood backstop.
- **Sim.** `meow test` → "litter swarm sim tests" (9): join, storm bound,
  direct reply, age discard, flake+cursor, mesh (3 litters), replay dedup,
  forged-signature rejection, permissive default. Real `HubState`, real
  `relay_jobs` and real per-agent signing over a mocked transport — each
  simulated agent has its own derived key, and messages are signed as the
  agent that says them.
- **Raft log.** Each `litter live` process appends state transitions and
  relay traffic to `/tmp/meow/<session-id>/raft.log`, next to where its
  conversation would sit — best-effort, never blocking the raft.

## Context: why the stopgap is not the destination

The first cross-litter bridge (2026-09-20, docker yard ↔ ryzen) is a
**hub-to-hub proxy**: each litter's hub keeps a relay log of outbound chat
and re-sends it into the peer's hub socket as ordinary `Send` traffic
(`live::relay_tick`). It works, but it is explicitly a stopgap, and it
inherits every problem of the thing it proxies:

- **Contingent on proxy hubs.** The relay rides on each side's hub socket —
  i.e., on whichever agent happens to hold the bind. A hub that stalls
  (observed live: a leader deaf for the duration of an LLM turn; a state
  lock held across deadline-bounced I/O) silences the entire bridge, not
  just one node. A proxy hub is a SPOF glued onto a SPOF.
- **No end-to-end picture.** Each hub relays what it has; neither side
  knows what the other side has already seen, so dedup is cursor-shaped
  guesswork (`last_relay_ts`), and "relevant" is undefined.

## First live join: yard ↔ ryzen (2026-09-20) — where it got to

Both litters were stood up and **discovered each other**
(`[event] static peer ryzen discovered at …`), each agent generated and
persisted its own `litter_key`, and the relay fired for real: sherlock's
`raft.log` carries
`relay fail to=192.168.1.126:7700 from=root ot=…` — the operator's
roll-call message attempting to cross while the far side was restarting.

**No message has yet completed a crossing.** The relay path is exercised
up to and including the send; the far hub has not yet accepted one.

What the attempt cost, and what it bought — each of these is written up
where it belongs, listed here so the next session starts from the right
place:

| Finding | Where |
|---|---|
| Probe I/O under the hub lock — two litters deadlock each other | `LITTER_RAFT_LOOP.md` § "Key consequences" |
| Leader could not serve itself (TCP round trip to its own hub) | same |
| `CLONE_SETTLS` is per-target; amd64 had **no raft thread at all** | same, § threading |
| Firecracker has no NAT/forwarding; WiFi cannot bridge | same, § "Deployment topology" |
| Guest overwrites its own `authorized_keys` when it generates a host key | below, "Known traps" |

### Known traps (bit us; not yet fixed)

- **The guest clobbers `authorized_keys`.** On a boot where no host key
  exists on disk, the Akuma guest generates one and writes over
  `etc/sshd/authorized_keys`, locking out the key `amd64/mkdisk.sh`
  staged into the image. Recover by writing the `.pub` back in with
  `debugfs -w -R "write <key>.pub etc/sshd/authorized_keys"` **with the VM
  stopped**. It does not recur once a host key is on disk (the boot log
  then says `Loaded host key from filesystem` rather than
  `Generating new host key`).
- **Never `debugfs -w` a disk a running VM has mounted rw**, and never
  `pkill` Firecracker with the rootfs mounted rw. Doing both on
  2026-09-20 left the image unable to `spawn '/bin/sh'` for ssh sessions;
  recovery is a rebuild via `amd64/mkdisk.sh`. The boot log on the host
  stays readable either way.
- **Do not rebuild a binary a live agent is running.** `litter/yard.sh`
  bind-mounts the host's `meow` into the container, so `cargo build`
  rewrites the text of every running agent and they die of SIGBUS — no
  `PANIC!` line, because it is a signal, not a Rust panic. Agents
  vanishing from `ps` with logs ending mid-line is this, not a litter
  bug. Stop the yard, rebuild, start it.

### Still to do

- Complete one verified crossing in each direction.
- Remove the leftover `DNAT` (`PREROUTING --dport 7700`) and the
  `MASQUERADE` for `10.0.2.0/24` on ryzen: the guest now has a real LAN
  address and needs neither.
- Both sides' configs still name pre-move addresses; the guest's
  `litter_hub_addr` and the yard's `litter_static_peers` need the guest's
  current LAN address.

## Direction (to review): raft-collected, peer-transmitted, storm-proof

The end state should NOT be contingent on proxy hubs. The properties we
want, in the order the user stated them:

1. **Messages collected per raft.** The raft layer — not whoever holds a
   socket — is the collector of record. Raft already has the machinery for
   "who is in, who leads, what order things happened in" (term, log,
   heartbeat). Outbound cross-litter traffic should be something raft
   *commits*, like any other log entry, so every agent converges on the
   same set of "messages that left the litter".

2. **Transmitted to peers — agents, not proxies.** Transmission happens
   agent-to-agent across litters (mesh), so any connected pair keeps the
   bridge alive and no single stalled process partitions two swarms.

3. **Irrelevant / already-transmitted discarded — no storms.** A joined
   swarm relays gossip; naive gossip is a broadcast storm. The discard
   rules are the design:
   - **Relay envelope (provenance + trackability).** A relayed message is
     not a bare body: it carries its **original timestamp** (origin hub's
     monotonic ts — already the relay cursor's ordering key), the
     **original sender's signature** (origin identity, not the relayer's
     claim of it), and the **relayer's signature** (who forwarded it, one
     per hop). Provenance chain falls out of this: any agent can answer
     "where did this come from, who carried it here" from the message
     alone, and a hub can reject envelopes whose relayer signature doesn't
     match a known peer.
   - **Metadata dedup** (built; the checksum this section originally
     specced turned out to be unnecessary): the message's identity is the
     `(origin litter, origin agent, origin ts)` triple. The agent has to
     be in there — `ot` is the sender's own clock now, so two agents in
     one litter can hand out the same microsecond, and keying on
     `(litter, ts)` alone would drop the second message as a duplicate.
     The seen-set also records the **relayer** (`rl`) for provenance; an
     echo of our own words is caught earlier, by `ol` matching our litter. A message already present is dropped on receipt AND on
     forward consideration. This is what kills loops — and it is why the
     first stopgap's `<litter>-<agent>` name prefixes are gone: a mutated
     prefix looked like a fresh sender and the mesh test caught the
     resulting storm (14 crossings for one message). Names on the wire are
     now always bare; provenance lives only in the envelope.
   - **Age discard:** messages older than X seconds (not yet seen) are
     dropped, not delivered — replaying stale debate into a freshly
     healed partition is exactly the storm shape we don't want. The
     seen-set + age bound together mean history resync happens through
     raft/compaction markers, never through replayed relay traffic.
   - **Relevance filter:** forwarding is not default-on. Candidates:
     addressed-to-me, addressed-to-my-litter, task traffic I hold a lease
     on, everything else folded (compaction already proves summaries beat
     transcripts for context economy — cross-litter relay should default
     to the same).
   - **Backpressure:** an unreachable peer never queues unboundedly (the
     relay log cap exists for this); a partition heals by resyncing the
     raft-committed set, not by replaying inboxes.

4. **Part of the raft traffic simulation.** This topology should be built
   and validated inside the existing raft traffic simulation: model a
   joined swarm (multiple litters, partial connectivity between them) and
   drive it with the real wire traffic until the agent graph settles into
   a **somewhat stable topology** — stable enough that message delivery is
   predictable, loose enough that a flapping member degrades relay instead
   of breaking it. Success criteria for the sim (draft):
   - no message delivered twice to the same agent (seen-set holds under
     churn);
   - no unbounded forwarding under any partition/rejoin pattern (storm
     bound: each message crosses any link at most once);
   - after a partition heals, both sides converge on the same committed
     set within one raft cycle;
   - topology converges (adjacency changes per minute → ~0) on a static
     membership.

## Open questions

- NOTE (cleanup): the stopgap relay clones aggressively — `relay_jobs`
  clones every job's from/to/body out of the relay log, and `handle`
  clones each captured message (`msg.clone()` per send). Fine at litter
  scale; revisit with cursors/refs or a queue of owned frames once the
  design settles.
- **Clock skew across `ot`.** Now that `ot` is the sender's own clock, it
  is compared against the receiver's clock in one place: `prune_seen`
  drops seen-entries older than `RELAY_MAX_AGE_US` measured locally. A
  peer whose clock runs behind ours has its entries forgotten early (a
  replay inside the window could be delivered twice); one running ahead
  keeps them longer than needed. Harmless on a LAN with roughly-synced
  clocks, wrong in principle — the fix is a received-at stamp for pruning,
  keeping `ot` purely as identity. The relay *age guard* is unaffected: it
  compares our own hub stamps.
- Does cross-litter relay live in the raft log itself (relay as committed
  entries) or beside it (raft-acknowledged outbox)? Log-resident is
  simpler to reason about; outbox keeps the log small.
- Identity: name prefixes are retired (see metadata dedup above) — an
  agent is addressed by its **bare** name and the envelope says which
  litter it came from. Two litters with an agent of the same name still
  collide on direct addressing: `Send { to: "al" }` relays to every online
  peer and lands in whatever `al` each one has. Stable per-agent ids that
  survive re-homing across litters would fix that; the signed `ol` fixes
  only the origin half.
- What is "irrelevant" formally — per-agent (inbox-style) or per-link
  (policy on the edge), and who decides?
- Does `litter-hub` (the standalone std hub) get the relay at all, or is
  it retired once agents relay directly?

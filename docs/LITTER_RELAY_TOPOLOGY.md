# Litter relay topology: where the cross-litter message plane goes

Status: OPEN DESIGN — to be reviewed. Nothing in here is built yet except
the stopgap noted below. Companion to `docs/LITTER_STATE_MACHINE.md` and
`docs/LITTER_RAFT_LOOP.md` (which see for states and traffic).

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
   - **Checksum dedup:** the envelope's checksum (over origin id + body +
     origin ts) is the message's identity. Every node keeps a seen-set of
     checksums; a message already present is dropped on receipt AND on
     forward consideration. This alone kills loops — the reason the
     stopgap relay needs litter-name prefixes and cursor discipline.
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
- Does cross-litter relay live in the raft log itself (relay as committed
  entries) or beside it (raft-acknowledged outbox)? Log-resident is
  simpler to reason about; outbox keeps the log small.
- Identity: is `<litter>-<agent>` enough, or do agents need stable ids
  that survive being re-homed across litters?
- What is "irrelevant" formally — per-agent (inbox-style) or per-link
  (policy on the edge), and who decides?
- Does `litter-hub` (the standalone std hub) get the relay at all, or is
  it retired once agents relay directly?

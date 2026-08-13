# Commercial model — MIT intact

How `rusty_dds` earns money **without** the license ever becoming the product.

The short version: **the code is free and stays free; what a studio pays for is
certainty.** Certainty that a dial is set right for *their* content, certainty
that someone answers when it breaks, and certainty that a number in a report was
measured properly. None of that requires withholding a line of source.

---

## 1. The promise

These are commitments, not marketing. They are what "intact" means.

1. **100% of the shipping code path is MIT.** Every format, every optimization,
   every RDO mode. There is no paid tier of the library, no "enterprise
   encoder", no feature held back to create a funnel.
2. **The license will not change.** No relicensing, no future BSL/SSPL/"fair
   source" migration, no open-core rug-pull. If this project is ever transferred
   or wound down, it stays MIT.
3. **No copyright assignment.** Contributions come in under a
   [DCO](https://developercertificate.org/)-style sign-off, not a CLA that
   assigns rights to us. We cannot relicense what we do not own — which is
   precisely the point: promise (2) is enforced by structure, not by trust.
4. **Forking is a supported outcome.** Vendor it, patch it, ship it in a
   closed-source game with no notice to us and no fee. That is the deal. Use
   your own name for the fork (see [trademarks](../THIRD-PARTY-NOTICES.md#trademarks)).
5. **Measurement stays public.** The harnesses, the corpora provenance, and the
   losses are in the repo. We do not publish a number we cannot hand someone the
   command for.

## 2. What we never sell

Naming the traps is part of the commitment, because each one is a plausible
short-term revenue idea that would destroy the asset:

| Anti-pattern | Why it's off the table |
|---|---|
| **Crippled OSS / paid feature tier** | The adoption path is bottom-up — community tools, contractors, then studios. Every one of those links is a zero-budget decision. A paywall severs the funnel that makes the enterprise conversation possible at all. |
| **Dual licensing + CLA** | Requires assignment, kills outside contribution, and makes promise (2) unenforceable. |
| **Per-title or per-seat fees** | This is the incumbent's model and the one axis where we beat it outright. Adopting it means being a worse-funded version of a decade-tuned competitor. |
| **Relicensing later** | The single most reputation-destroying move available to an open project, and it is a one-time trade of all future trust for one quarter of revenue. |
| **Trademark overreach** | Marks protect *our name*, never the user's right to run, fork, or ship the code. |

## 3. What we sell

Five lines. Each sells work that cannot be copied out of the repository, because
the scarce input is measurement discipline and accountability — not source.

### 3.1 Corpus calibration

The flagship. A studio's texture mix is not our corpus: the right RDO λ per
texture class, the right quality floors, and the maps that should be left alone
are all content-specific. We take a representative pack under NDA and return the
rate-distortion curve on *their* assets, a per-class λ recommendation, a per-map
regression table, and a named list of maps where we advise leaving the feature
off.

*Unit of sale:* fixed-fee engagement per corpus.
*Why it's defensible:* the work is the campaign discipline — harvest, ceiling,
gate, revert — not the encoder. Someone who clones the repo still has to learn
which numbers are admissible.

### 3.2 Support & SLA

Named contact, guaranteed response window, security-advisory lead time, version
pinning with backported fixes, and a written answer to "who do we call at 2am
before submission?" This is the line that converts a pre-1.0 library from
*pilot-appropriate* to *procurement-appropriate*.

*Unit of sale:* annual subscription, tiered by response time.

### 3.3 Integration engineering

The C ABI shim so a C++ resource compiler calls it like any other library,
native split-mip / streaming-container readers, build-farm and CI wiring,
custom container or platform formats.

*Unit of sale:* scoped project, or day rate.
*Note:* deliverables land in the MIT tree by default. The client is paying for
**schedule and fit**, not exclusivity — and gets a maintained upstream feature
instead of a private patch they carry forever. Where a client genuinely needs a
private format, they keep that in their own tree.

### 3.4 Validation reports

A signed report: corpus conformance, round-trip gates, regression tables, and
the method line for every figure. Useful for platform submission confidence and
for internal sign-off when a texture pipeline changes.

*Unit of sale:* per report, or bundled into 3.2.

### 3.5 Sponsored roadmap

A studio funds a capability they need — BC3 RDO, ASTC, a container, a platform.
It lands **MIT for everyone**; the sponsor gets it first, shaped to their
pipeline, with their content as the calibration corpus.

*Unit of sale:* fixed-fee feature contract.
*Why a sponsor accepts public release:* they were never buying exclusivity, they
were buying *existence by a date* — plus permanent upstream maintenance instead
of a fork they own forever.

## 4. Pricing

Deliberately not set in this document. Rates are a business decision, not an
engineering one. Two structural notes:

- **Sell outcomes, not hours, where the outcome is measurable.** "λ calibrated
  across your BC1/BC7 classes with a per-map regression table" is a fixed-fee
  deliverable. Support is a subscription. Only integration work should be a day
  rate.
- **Anchor calibration value on shipped bytes, not on effort.** The saving
  recurs on every patch for every player; effort does not. The
  [pitch calculator](https://claude.ai/code/artifact/534c7e13-e768-4019-a097-bf81c846d9c9)
  exists so a prospect derives that number themselves rather than hearing it
  from us.

## 5. Honest limits of this model

- **Revenue scales with people, not installs.** Services income is linear in
  headcount; a license is not. This model funds a small excellent team, not a
  hypergrowth curve. That is a deliberate choice, and it should be made with
  eyes open.
- **The first support contract is the hard one.** Pre-1.0 with no reference
  customer is a real objection; the answer is pilots (see the pitch's three
  steps), not discounting.
- **Nothing here is legal advice.** The trademark boundary, the DCO mechanics,
  and any support agreement need review by a lawyer before signature. This
  document states intent and structure; it is not a contract.

---

*Companion documents:* [`LICENSE-MIT`](../LICENSE-MIT) ·
[`THIRD-PARTY-NOTICES.md`](../THIRD-PARTY-NOTICES.md) ·
[`docs/plans/texture-pipeline.md`](plans/texture-pipeline.md)

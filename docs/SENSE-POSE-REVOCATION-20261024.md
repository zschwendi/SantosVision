# Independent Sense source loss

The fresh build 20 runtime report at
`target/verification/device-session-20261024/tracked-aim21/feedback-runtime-report.json`
proves that both actual accessory providers were authorized/running and native
exact-head reads accepted both full poses. Its latest immersive accepted packet
has flags 3, zero packet/presentation delta and original right source age -3.72 ms
within the existing 5 ms future tolerance. Native counters record 1,166 left and
1,133 right applied targets. These are historical facts from before immersion
closed, not proof of visible movement or correct arm rendering.

An independent acquisition defect was confirmed in source: losing either hand
called whole-owner invalidation, erased the healthy hand's exact-head snapshots
and skipped publishing that healthy hand for the new head. This now uses a
per-hand revocation fence. Stored packet identities/times remain immutable;
reads clear only the revoked hand. A sampling operation captures each hand's
generation before querying ARKit, so an in-flight query cannot resurrect a
source lost during the query. Healthy current-head data still publishes. Head,
session and owner loss keep whole-epoch invalidation. An ARKit read excluded by
an accessory-update barrier is not interpreted as observed source loss; its
new head still receives no fabricated unqueried pose.

Freshness limits, named grip/aim transforms, full position/orientation tracking,
exact head lookup, authorization and the original 80-float C ABI remain. The
ordinary DualSense is excluded from spatial accessory discovery; no pose or
barrel direction is invented from buttons or a bare wrist. Native conversion
uses named aim -Z as barrel +X; transport does not guess a grip forward axis.

The runtime report now includes at most two `named_accessory_source_samples`
records: original/target timestamps, exact accepted head/transport epoch, actual
stage grip XYZ and aim -Z, sample count, and maximum displacement/rotation from
the first accepted pose in a continuous origin interval. Extents reset on
head/session-origin invalidation. Records survive immersion exit for diagnosis
and explicitly do not establish current tracking authority. The next headset
check must show these actual pose values changing and compare them to native
live target/bone values; hook counts alone do not establish visible arm motion.

Validation: 16 host Swift 6 policy/transport tests cover independent loss of
either hand, in-flight revocation, healthy current-head publication, recovery
without historical resurrection, bare wrist behavior, update barriers, global
owner/session fences, original source freshness and C ABI layout. Full physical
XROS 27 SDK / arm64-xros26 strict Swift 6 typecheck passes. Artifacts are in
`target/verification/device-session-20261024/tracked-aim21/swift-source-revocation/`.
No device build or physical arm/aim acceptance is claimed by this change.

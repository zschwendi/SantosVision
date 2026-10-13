# GTA save history for SantosVision

`SantosSaveSync` mirrors the game's local profile files under
`Documents/saves/userdata/Documents/Rockstar Games/GTA V/Profiles/`. The native
engine continues to use its local `Documents/saves` root. The synchronizer does
not include `native-graphics.json`, caches, source game data, or the sibling
`settings.xml` graphics configuration. `src/config.rs` confirms the GTA V
`userdata/Documents/Rockstar Games/GTA V` mapping; the profile directory is
the GTA V save convention and still needs confirmation against a physical
SantosVision gameplay save.

## App integration

Create one `SantosSaveSync` actor per game session. Put `archiveRoot` under
Application Support, outside `saveRoot` and game data. Pass the actual signed
iCloud container ID:

```swift
let sync = SantosSaveSync(configuration: .live(
    saveRoot: model.saveRoot,
    archiveRoot: applicationSupport.appendingPathComponent("SantosVision/SaveSync"),
    containerIdentifier: "iCloud.<provisioned SantosVision container>"))
let result = try await sync.restoreBeforeLaunch()
// Only now call NativeRuntime.start(...).
await sync.markGameplayStarted()
```

If prelaunch discovery or verification fails, surface the error and keep local
saves; do not start gameplay against a partly restored profile. During gameplay,
call `snapshotAndUpload()` on a bounded periodic timer and after a user pause.
At quit, stop native writes, await a final `snapshotAndUpload()`, then call
`markGameplayStopped()`. An OS termination cannot promise a final upload, which
is why periodic snapshots matter. The method uses two matching content scans
separated by 350 ms and reports changing files for retry. It does not restore
or overwrite any gameplay file while the engine is running.

The cloud files live at
`<container>/Documents/SantosVisionSaveSync/v1/revisions/<UUID>.svsave`.
Each revision is a self-contained, hash-verified snapshot. Revisions are
written under unique names, with a staged file moved to its final name; an
existing revision is verified and never replaced. The local Application
Support archive stores the same revisions and a small atomic state journal.
If an app interruption occurs during a prelaunch restore, the next prelaunch
finishes the journaled revision from the local archive before reading iCloud.

The initial cloud scan uses `NSMetadataQuery` rather than treating an empty
File Provider listing as a genuinely empty account. All discovered cloud
revisions must finish downloading and pass hashes before automatic restore.
When a local save changed independently, automatic restore keeps the local
files and reports `divergentRemoteRevisions`; both branches remain available
in immutable history. A recovery UI can call `availableRevisions()` and, only
before engine startup, `restoreRevisionBeforeLaunch(_:)`. That explicit
restore first archives the current local snapshot.

## Capability and acceptance

The signed app needs its own iCloud Documents container in the Apple Developer
account, matching `com.apple.developer.icloud-services = CloudDocuments`,
`com.apple.developer.icloud-container-identifiers`, and
`com.apple.developer.ubiquity-container-identifiers`. The same container ID
must be passed to `.live(...)`. The current `visionos/project.yml` has no
iCloud entitlement and no existing container is inferred from the bundle ID.
Provisioning and physical Vision Pro testing are integration gates. Do not
claim cloud transport works from a host-only test.

Host verification, without simulator or device:

```sh
swiftc -swift-version 6 -strict-concurrency=complete \
  visionos/Sources/SantosSaveSync.swift \
  visionos/Sources/SantosSaveCloudDiscovery.swift \
  visionos/Tests/SantosSaveSyncHostTests.swift \
  -o /tmp/santos-save-sync-host-tests
/tmp/santos-save-sync-host-tests
```

The synthetic host test covers immutable publication, first-device restore,
newer descendant restore, divergent branches, explicit recovery, malformed
cloud history, offline behavior, and the gameplay restore guard. The files
also pass visionOS SDK Swift 6 typechecking. Physical acceptance must confirm
the actual profile path, iCloud account/provisioning, cross-device download
latency, pause/quit persistence, and recovery after an interrupted upload.

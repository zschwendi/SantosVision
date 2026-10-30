# Maintainer IPA packaging

The preview repackages an existing, identified visionOS build. It does not
recompile the runtime or install on a device. The input is left untouched.
`packaging/sideload.json` records the original source revision, exact allowed
bundle files, input hashes, explicit exclusions and packaging changes.

On a Mac with Xcode command-line tools, from the repository root:

```sh
python3 tools/package_sideload.py --app /path/to/Reviewed.app --output release-output/new-preview
```

The input must match the reviewed recipe, including every included file's
SHA-256. A different build requires a newly reviewed recipe; do not just remove
the hash checks. Unknown top-level entries and symlinks are rejected. Source
code archives and arbitrary developer app directories are not release inputs.

The packager creates `Payload/App.app`, removes the old code signature and
provisioning profile by excluding signing files, and applies an ad-hoc
signature with the recipe's capabilities. These packages use local saves and
do not carry the maintainer's iCloud entitlements. Recipients must sign them
again with a profile for their own headset.

Outputs are the `.ipa`, `manifest.json` (all packaged files, hashes and
provenance), and `SHA256SUMS`. ZIP integrity and strict ad-hoc signature checks
run before reporting success. The input hashes are checked again at the end.
Never add the output directory to Git. Attach reviewed files to a GitHub
**prerelease**, with the exact guide link, version, known limits and validation
status. The release tag should include the packaging recipe and guide.

Packaging checks are not fresh-account installation or gameplay checks.
Before calling a release fully supported, test another signing account on
Windows, initial installation, owned-data import, windowed play, expiry/refresh
and an update preserving saves. Record these separately; do not overwrite the
maintainer's known-good app to simulate a fresh installation.

# Releasing

Releases are built and published by [`.github/workflows/release.yml`](../.github/workflows/release.yml). Every push and pull request is also built by [`ci.yml`](../.github/workflows/ci.yml).

## Making a release
1. On a `release/vX.Y.Z` branch, set `version` in `Cargo.toml`, run `cargo build` (updates `Cargo.lock`), commit, then merge into `main`.
2. Tag `main` and push the tag:
   ```powershell
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```
3. The **Release** workflow:
   - builds the exe on a GitHub-hosted runner
   - checks that the tag matches the version in `Cargo.toml`
   - uploads the unsigned exe as a workflow artifact
   - sends a signing request to SignPath and waits for it
   - checks the signature
   - creates the GitHub release with the signed exe
4. **Approve the signing request** in SignPath (you also get an email). The workflow waits up to 4 hours; after that it fails, and you can run it again from the Actions tab with **Run workflow** and the tag.

Until SignPath is set up, the workflow publishes the exe unsigned and shows a warning.

## Setting up SignPath (one time)
1. **Apply** for the free open-source program at <https://signpath.org/apply>. Requirements met by this repo:
   - OSI license (MIT)
   - already released
   - functionality described in the README
   - a "Code signing policy" section in the README
   - built from source by GitHub Actions

   Turn on two-factor sign-in for both GitHub and SignPath.
2. After approval, in the SignPath web app:
   - **Trusted build systems:** add the predefined *GitHub.com* system and link it to the project.
   - **Project** slug: `Audio-Output-Device-Switcher`. Repository URL: `https://github.com/GerdonAbbink/Audio-Output-Device-Switcher`.
   - **Artifact configuration:** paste [`.signpath/artifact-configuration.xml`](../.signpath/artifact-configuration.xml) and make it the default.
   - **Signing policy** slug: `release-signing`.
     - Use the approval process, with yourself as approver.
     - Add the CI user as submitter.
     - If you limit the allowed branches/refs, include the release tags (`refs/tags/v*`), because releases are built from tags.
   - **CI user / API token:** create a CI user that can submit to `release-signing` and create an API token for it.
3. In GitHub (Settings → Secrets and variables → Actions), or with the GitHub CLI:
   ```powershell
   gh secret set SIGNPATH_API_TOKEN --repo GerdonAbbink/Audio-Output-Device-Switcher
   gh variable set SIGNPATH_ORGANIZATION_ID --repo GerdonAbbink/Audio-Output-Device-Switcher --body "<organization id>"
   ```
   If you choose a different project or signing policy slug, change `SIGNPATH_PROJECT_SLUG` / `SIGNPATH_SIGNING_POLICY` at the top of `release.yml`.
4. Optionally install the [SignPath GitHub App](https://github.com/apps/signpath) on the repository.

## Version info
The exe's product name, version and copyright are embedded by [`build.rs`](../build.rs). SignPath checks that the product name is `Audio Output Device Switcher` and the product version equals the `Cargo.toml` version, so keep these in sync if you rename the product.

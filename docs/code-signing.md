# Code signing

The release workflow signs `rsrewind.exe` and the MSI with Azure Artifact Signing using the
active `jdspille` / `primary-profile` Public Trust certificate profile. Both signatures must
verify with an RFC 3161 timestamp and the `CN=Joseph Spillers` subject before a release can
publish. The ZIP is assembled after signing the EXE, and the MSI packages that signed EXE.

The `release` GitHub environment holds the signing endpoint, account, profile, expected
subject, and Azure client, tenant, and subscription IDs. Its deployment policy allows `main`
and `v*` tags. The Azure app `rsrewind-release-signing` has a federated credential for this
repository's immutable OIDC subject and the Certificate Profile Signer role on this profile
only. No long-lived Azure client secret is stored in GitHub.

To repair or reproduce the setup, sign in with `az login` and `gh auth login`, then run:

```powershell
./scripts/setup-signing.ps1 -AccountName jdspille -ResourceGroup RG0 -ProfileName primary-profile
```

The script requires Azure rights to create app registrations and assign roles, plus GitHub
repository admin rights. It checks the profile is active, uses this repository's actual OIDC
subject, and is safe to rerun. It does not create the signing account or perform Azure identity
validation.

To check signing without publishing, run the `Release` workflow from `main` with
`dry_run=true`. The signed EXE, ZIP, and MSI are uploaded as workflow artifacts; no GitHub
Release is created. A real `vX.Y.Z` tag uses the same signing steps and publishes a prerelease
only after both signatures and checksums pass.

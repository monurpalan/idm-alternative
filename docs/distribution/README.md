# Distribution

Texts and files for listing Shardload outside GitHub. Nothing here is submitted by itself.

| What | File | Where it goes |
|---|---|---|
| Privacy policy | [../PRIVACY.md](../PRIVACY.md) | The stores' privacy policy URL, AlternativeTo |
| Extension store texts (EN/TR), permission justifications, data usage | [store-listing.md](store-listing.md) | Chrome Web Store, Microsoft Edge Add-ons, Firefox Add-ons |
| Screenshots, 1280×800 | [screenshots/](screenshots) | The stores, AlternativeTo |
| winget manifests | [winget/manifests/m/monurpalan/Shardload/](winget/manifests/m/monurpalan/Shardload) | A pull request to [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) at the same path |
| AlternativeTo | [alternativeto.md](alternativeto.md) | alternativeto.net, "Suggest an application" |

## winget

The manifests are for the version named in their folder; `InstallerSha256` is the SHA-256 of
`Shardload_<version>_x64-setup.exe` in that release's `SHA256SUMS`. To check them, and try the
install from them, on Windows:

```
winget validate docs\distribution\winget\manifests\m\monurpalan\Shardload\0.1.7
winget install --manifest docs\distribution\winget\manifests\m\monurpalan\Shardload\0.1.7
```

(`winget settings --enable LocalManifestFiles` once, as administrator, allows the second.)
For a new version, `wingetcreate update monurpalan.Shardload --version <v> --urls <installer url>`
writes the next set once the package is in winget-pkgs.

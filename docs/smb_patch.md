# SMB Auth Patch

This project currently uses a patched checkout of your forked `smb-rs` repo at:

- [smb-rs](/home/xuranus/workspace/fpt/smb-rs)

The active Cargo override in [Cargo.toml](/home/xuranus/workspace/fpt/Cargo.toml) is:

```toml
[dependencies]
smb_client = { package = "smb", version = "0.11.1", default-features = false, features = ["async", "sign", "encrypt", "compress"], optional = true }
sspi = { version = "0.19.2", optional = true }

[patch.crates-io]
smb = { path = "smb-rs/crates/smb" }
```

## Why This Patch Exists

The stock crate did not authenticate successfully against the local Samba server used for development.

Observed behavior:

1. Without the required SMB protocol/security features enabled, negotiate failed.
2. After enabling those features, session setup still failed in the upstream auth path.

The blocker was inside `smb-rs` authentication, not only in Fpt's usage of it.

## Exact Patch Applied To Your Fork

Patched file:

- [smb-rs/crates/smb/src/session/authenticator.rs](/home/xuranus/workspace/fpt/smb-rs/crates/smb/src/session/authenticator.rs)

Behavioral changes:

1. Switch from SPNEGO `Negotiate` to direct `Ntlm`.
2. Switch the credential handle storage from `CredentialsBuffers` to `AuthIdentityBuffers`.
3. Pass `AuthIdentity` directly into `with_auth_data(...)`.
4. Change SSPI target name from `cifs/<host>` to just `<host>`.
5. Always set `with_target_name(...)` on the security context builder.
6. Remove `get_available_ssp_pkgs(...)` and the `AuthMethodsConfig`-driven SPNEGO package selection from this path.

In diff form, the important transitions are:

- `Negotiate` -> `Ntlm`
- `AcquireCredentialsHandleResult<Option<CredentialsBuffers>>` -> `AcquireCredentialsHandleResult<Option<AuthIdentityBuffers>>`
- `with_auth_data(&sspi::Credentials::AuthIdentity(identity.clone()))` -> `with_auth_data(&identity)`
- `format!("cifs/{server_fqdn}")` -> `server_fqdn.to_string()`

## Fpt-Side Dependency Changes

Two repo-side changes were required:

1. Enable the SMB crate features that the working path depends on:
   - `async`
   - `sign`
   - `encrypt`
   - `compress`
2. Align Fpt's direct `sspi` dependency with the forked SMB crate:
   - `sspi = "0.19.2"`

That version alignment matters because [src/bin/smbprobe.rs](/home/xuranus/workspace/fpt/src/bin/smbprobe.rs) constructs `sspi::AuthIdentity` directly. If Fpt and `smb-rs` pull different `sspi` versions, the types are incompatible and `cargo build` fails.

## Current Validation

Validated locally with:

```bash
cargo build --bin fptcli --bin smbprobe --features smb --features nfs
cargo test --lib --features smb --features nfs
./target/debug/smbprobe --target 'smb://127.0.0.1/dataset/out?username=xuranus&password=123456789'
```

The `smbprobe` check succeeded through:

- connect
- authenticate
- tree connect
- open root

## Recommended Dependency Modes

### Mode 1: Local Fork Checkout

Current working setup:

```toml
[patch.crates-io]
smb = { path = "smb-rs/crates/smb" }
```

Use this while you are still editing the fork locally.

### Mode 2: GitHub Fork Pin

Recommended repo state after you commit and push the fork:

```toml
[patch.crates-io]
smb = { git = "https://github.com/XUranus/smb-rs", rev = "<commit>" }
```

This keeps third-party source out of the Fpt repo while still pinning an exact working revision.

## How To Move From Local Path To GitHub Fork

1. Commit the patch inside [smb-rs](/home/xuranus/workspace/fpt/smb-rs).
2. Push it to `https://github.com/XUranus/smb-rs`.
3. Replace the local path override in [Cargo.toml](/home/xuranus/workspace/fpt/Cargo.toml) with the `git` + `rev` form above.
4. Run:

```bash
cargo build --bin fptcli --bin smbprobe --features smb --features nfs
cargo test --lib --features smb --features nfs
```

## Should `vendor/smb` Be Kept?

No, not if your fork is now the source of truth.

Once the fork is committed and pushed, the in-repo vendor copy is unnecessary overhead. The cleaner options are:

1. local path override while iterating
2. pinned GitHub fork for normal development/CI
3. upstream the fix later and remove the patch entirely

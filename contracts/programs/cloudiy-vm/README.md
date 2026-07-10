# cloudiy-vm — identity-keyed VM manifest

A tiny Anchor program: one PDA per wallet (`["vm", owner]`) holding a small
manifest (installed apps, rented machine, VM name). The PDA is derived from the
owner and the owner must sign, so an identity can only write its **own**
manifest — decentralised, keyless, no central server. This is where CloudiyOS
state lives so the same wallet resumes on **any browser or device**.

Large / regenerable data (model output samples) stays off-chain. If the
manifest ever needs to carry big blobs, it holds a pointer + content hash to an
encrypted blob on decentralised storage (e.g. Arweave/Irys, signed by the same
Solana wallet) — the UI's sync layer already has the blob-store and client-side
encryption seams stubbed for that (`CloudiyVM.blobStore` / `deriveKey`).

## Deploy (devnet)

```bash
cd contracts
anchor build                 # generates the program keypair + id
anchor keys sync             # writes the real id into declare_id! + Anchor.toml
anchor deploy --provider.cluster devnet
```

`declare_id!` and `Anchor.toml` currently hold a **placeholder** id
(`1111…1111`); `anchor keys sync` replaces it with the real deploy key.

## Enable the sync in the UI

The browser sync is **dormant** until it knows the program id — until then the
app behaves exactly as before (state in `localStorage`). Point it at the
deployed program with either:

- `?vmprog=<PROGRAM_ID>` in the URL (persisted), or
- `localStorage.setItem('cloudiy_vm_prog', '<PROGRAM_ID>')`,

or bake the id into `web/vm.html` (`CloudiyVM` `PLACEHOLDER`/`PROGRAM_ID`). Once
set, on login the UI pulls the on-chain manifest, union-merges it with local
state, and pushes changes back (debounced, wallet-signed).

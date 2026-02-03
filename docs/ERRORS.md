# Known errors

## staging directories

Staging a directory produces a wrong commit because we don't stage the files correctly, example:

```
akuma:/meow> scratch commit -m 'thinking mode docs'
scratch: committing changes...
[commit] 1 staged file(s)
[commit]   staged: docs/
[commit] Merging with parent c5f848b7cbe2fe6d713043c0febd15b6d912609e
[commit] Parent tree has 6 entries:
[commit]   file: Cargo.toml (10973f8c2fc76c613beed3748ed6ec7aaecc1ad9)
[commit]   file: MEOW.md (c4263afd208f565236b9bc16da3df4b2b38f4b46)
[commit]   dir: docs (8e57400cb429ae3183c931f3041955568659c9ae)
[commit]   file: meow.js (c3b0cc0084748ac343d2a05bf7e4493d2d741645)
[commit]   dir: prompts (7d32e3f3e04d9d49fbb3acd1b3ed1a1fdac6fceb)
[commit]   dir: src (eecbdfb79d1f2b276173895e13b39bd5f2f86d52)
[commit] Result tree has 6 entries:
[commit]   file: Cargo.toml (10973f8c2fc76c613beed3748ed6ec7aaecc1ad9)
[commit]   file: MEOW.md (c4263afd208f565236b9bc16da3df4b2b38f4b46)
[commit]   dir: docs (36973bc3885aaf9bd3dd816563b13745bd0184b0)
[commit]   file: meow.js (c3b0cc0084748ac343d2a05bf7e4493d2d741645)
[commit]   dir: prompts (7d32e3f3e04d9d49fbb3acd1b3ed1a1fdac6fceb)
[commit]   dir: src (eecbdfb79d1f2b276173895e13b39bd5f2f86d52)
scratch: created commit 0dcf7685f30491f7d2d4814336eb58a73bc07f35
akuma:/meow> git push
Unknown command: git
Type 'help' for available commands.
akuma:/meow> scratch push
scratch: pushing to origin
scratch: pushing branch main
scratch: fetching refs for push from /netoneko/meow.git/info/refs?service=git-receive-pack
scratch: c5f848b -> 0dcf768
scratch: packing 25 objects
scratch: pack size 49116 bytes
scratch: pushing to /netoneko/meow.git/git-receive-pack
scratch: push failed: push failed with status 500
[exit code: 1]
akuma:/meow> scratch push
scratch: pushing to origin
scratch: pushing branch main
scratch: fetching refs for push from /netoneko/meow.git/info/refs?service=git-receive-pack
scratch: c5f848b -> 0dcf768
scratch: packing 25 objects
scratch: pack size 49116 bytes
scratch: pushing to /netoneko/meow.git/git-receive-pack
scratch: push failed: push failed with status 500
[exit code: 1]
akuma:/meow> scratch push
scratch: pushing to origin
scratch: pushing branch main
scratch: fetching refs for push from /netoneko/meow.git/info/refs?service=git-receive-pack
scratch: c5f848b -> 0dcf768
scratch: packing 25 objects
scratch: pack size 49116 bytes
scratch: pushing to /netoneko/meow.git/git-receive-pack
remote: error: empty filename in tree entry
remote: error: object 36973bc3885aaf9bd3dd816563b13745bd0184b0: badTree: cannot be parsed as a tree
fatal: fsck error in packed object
scratch: push complete
```

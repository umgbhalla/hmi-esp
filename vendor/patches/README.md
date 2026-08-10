# Patch overlay

This directory is reserved for minimal patches that cannot live in product
code. Upstream trees must remain clean.

To capture a change:

```sh
git -C vendor/upstream/<directory> diff --binary > \
  vendor/patches/<directory>/0001-description.patch
```

Then add its relative path to `series` in application order. Every patch must
state why an adapter in product code was insufficient and which pinned commit
it applies to.


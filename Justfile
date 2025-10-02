# Run all recipes with Nushell
set shell := ["nu", "-c"]

# ── helpers ───────────────────────────────────────────────────────────────────

# Print current crate version (as seen by Cargo)
version:
	#!/usr/bin/env nu
	cargo metadata --no-deps --format-version 1
	| from json
	| get packages.0.version
	| str trim
	| print $in

# Ensure no uncommitted changes unless ALLOW_DIRTY=true
ensure-clean:
	#!/usr/bin/env nu
	if (($env.ALLOW_DIRTY? | default "false" | str downcase) == "true") {
		print "⚠️  ALLOW_DIRTY=true: skipping clean-tree check"
	} else {
		if (git status --porcelain | is-empty) {
			print "✓ Working tree clean"
		} else { print "❌ Working tree not clean. Commit or set ALLOW_DIRTY=true"; exit 1 }
	}

# Ensure cargo-edit is present (for cargo set-version)
ensure-cargo-edit:
	#!/usr/bin/env nu
	if (which cargo-set-version | is-empty) {
		cargo install cargo-edit --quiet --force
	}

# Run quick checks before publish
preflight:
	cargo check
	cargo test -q
	cargo package -q

# ── main entrypoints ──────────────────────────────────────────────────────────

# Bump {patch|minor|major|<semver>}, commit, tag, publish to crates.io, push to GitHub
# Usage:
#   just release patch
#   just release minor
#   just release major
release level:
	#!/usr/bin/env nu
	$env.LIBTORCH = '/opt/homebrew/Caskroom/miniconda/base/pkgs/pytorch-2.4.0-py3.11_0/lib/python3.11/site-packages/torch'
	$env.DYLD_LIBRARY_PATH = $"($env.LIBTORCH)/lib"
	just ensure-clean
	just ensure-cargo-edit

	# bump version
	cargo set-version --bump {{level}}

	# read bumped version
	let version = (just version | str trim)
	print $"🔼 Bumping to v($version)"

	# commit & tag
	git add Cargo.toml Cargo.lock
	git commit -m $"Release v($version)"
	git tag $"v($version)"

	# sanity checks (build/tests/package), then a dry-run publish
	just preflight
	print "🚀 Dry run publish…"
	cargo publish --dry-run

	# real publish
	print "📦 Publishing v($version) to crates.io…"
	cargo publish

	# push branch & tag
	print "⬆️  Pushing to GitHub…"
	git push origin main
	git push origin $"v($version)"

# Convenience shorthands
patch:
	just release patch

minor:
	just release minor

major:
	just release major

# Publish the current version without bumping
publish-only:
	just ensure-clean
	just preflight
	cargo publish

{
  description = "derisk: an adaptive, agent-first Wayland desktop shell built on mcsapi";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # The toolchain comes from nixpkgs (rustc there is newer than the
    # workspace's rust-version), so its binary cache covers rustc, clippy and
    # rust-analyzer. crane has no inputs of its own, so it needs no `follows`.
    # mcsapi is not an input: Cargo.lock pins its git revision and crane
    # vendors it from there.
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
    }:
    let
      inherit (nixpkgs) lib;
      # Wayland only: Smithay is Linux-only.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      perSystem =
        pkgs:
        let
          craneLib = crane.mkLib pkgs;

          # Cargo sources plus runtime and test data.
          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./crates/derisk/data
              ./crates/derisk-portal/data
              ./crates/derisk-apps/data
              (lib.fileset.fileFilter (f: f.hasExt "rs" || f.name == "Cargo.toml") ./crates)
              # The palette's and the overview's plugins, which
              # derisk-plugin's build script compiles to WebAssembly, and the
              # interfaces they are built against.
              (lib.fileset.fileFilter (f: f.hasExt "rs" || f.name == "Cargo.toml") ./plugins)
              ./crates/derisk-plugin/wit
            ];
          };

          # Linked at build time by Smithay and eframe.
          buildLibs = with pkgs; [
            libxkbcommon
            wayland
            libGL
            # The lock screen authenticates through PAM.
            linux-pam
            # mcsapi-compositor's bare-seat backend: libinput for input, udev
            # to find the GPU, GBM and libdrm for scanout, libseat for the
            # seat itself.
            libinput
            udev
            libgbm
            libdrm
            seatd
            # derisk-machines talks to libvirt through its own C library.
            libvirt
          ];

          # dlopen'd at run time by winit, Smithay's EGL renderer and eframe.
          runtimeLibs = with pkgs; [
            libGL
            libxkbcommon
            wayland
            libx11
            libxcursor
            libxi
            libxrandr
            libxcb
          ];

          commonArgs = {
            inherit src;
            strictDeps = true;
            pname = "derisk-workspace";
            version = "0.1.0";
            nativeBuildInputs = [
              pkgs.pkg-config
              # nixpkgs' rustc carries wasm32-unknown-unknown's std, for the
              # palette plugins, but links it with lld from PATH rather than
              # a bundled rust-lld.
              pkgs.lld
            ];
            buildInputs = buildLibs;
            LD_LIBRARY_PATH = lib.makeLibraryPath runtimeLibs;
            # The checks build with the dev profile (below); without debug
            # info its test binaries are a fraction of the disk and memory.
            CARGO_PROFILE_DEV_DEBUG = "0";
          };

          # The CI matrix (default and host) plus the apps' preview window.
          variants = {
            default = "";
            host = "--features derisk/host,derisk-apps/preview";
          };

          depsFor =
            profile: features:
            craneLib.buildDepsOnly (
              commonArgs
              // {
                pname = "derisk-workspace";
                CARGO_PROFILE = profile;
                cargoExtraArgs = "--locked --workspace ${features}";
              }
            );
          # Clippy and the tests build with the dev profile, as the check job
          # does. Release is fat LTO with one codegen unit, and linking every
          # test binary that way, each carrying wasmtime for the palette's
          # plugins, beside the two packages' own links had the CI runner
          # shut down mid-build.
          deps = lib.mapAttrs (_: depsFor "dev") variants;
          releaseDeps = depsFor "release" variants.host;

          variantChecks = lib.concatMapAttrs (
            name: features:
            let
              args = commonArgs // {
                pname = "derisk-workspace";
                CARGO_PROFILE = "dev";
                cargoArtifacts = deps.${name};
                cargoExtraArgs = "--locked --workspace ${features}";
              };
            in
            {
              "clippy-${name}" = craneLib.cargoClippy (
                args
                // {
                  cargoClippyExtraArgs = "--all-targets -- -D warnings";
                }
              );
              "test-${name}" = craneLib.cargoTest args;
            }
          ) variants;

          # Binaries find the GL, Wayland and X11 libraries they dlopen.
          withRuntimeRpath = ''
            for bin in $out/bin/*; do
              patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} "$bin"
            done
          '';

          packages = rec {
            derisk = craneLib.buildPackage (
              commonArgs
              // {
                pname = "derisk";
                cargoArtifacts = releaseDeps;
                cargoExtraArgs = "--locked -p derisk -p derisk-portal --features derisk/host";
                doCheck = false;
                postInstall = ''
                  install -Dm644 -t $out/share/systemd/user crates/derisk/data/systemd/user/* crates/derisk-portal/data/systemd/user/*
                  # Run this build's derisk, not whichever one is on the manager's PATH.
                  substituteInPlace $out/share/systemd/user/derisk-agent.service \
                    --replace-fail "ExecStart=derisk " "ExecStart=$out/bin/derisk "
                  # The portal backend: xdg-desktop-portal finds derisk.portal and the
                  # portals.conf under share/, and D-Bus needs an absolute Exec=.
                  install -Dm644 -t $out/share/xdg-desktop-portal/portals crates/derisk-portal/data/portal/derisk.portal
                  install -Dm644 -t $out/share/xdg-desktop-portal crates/derisk-portal/data/portal/derisk-portals.conf
                  install -Dm644 -t $out/share/dbus-1/services crates/derisk-portal/data/dbus-1/services/*
                  substituteInPlace $out/share/dbus-1/services/org.freedesktop.impl.portal.desktop.derisk.service \
                    --replace-fail "Exec=xdg-desktop-portal-derisk" "Exec=$out/bin/xdg-desktop-portal-derisk"
                  substituteInPlace $out/share/systemd/user/xdg-desktop-portal-derisk.service \
                    --replace-fail "ExecStart=xdg-desktop-portal-derisk" "ExecStart=$out/bin/xdg-desktop-portal-derisk"
                '';
                postFixup = withRuntimeRpath;
                meta.mainProgram = "derisk";
              }
            );
            derisk-preview = craneLib.buildPackage (
              commonArgs
              // {
                pname = "derisk-preview";
                cargoArtifacts = releaseDeps;
                cargoExtraArgs = "--locked -p derisk-apps --features preview --bin derisk-preview";
                doCheck = false;
                postFixup = withRuntimeRpath;
                meta.mainProgram = "derisk-preview";
              }
            );
            default = derisk;
          };
        in
        {
          inherit packages;

          checks =
            variantChecks
            // {
              fmt = craneLib.cargoFmt {
                inherit (commonArgs) src pname version;
                cargoExtraArgs = "--all";
              };
            }
            // lib.mapAttrs' (name: lib.nameValuePair "package-${name}") (removeAttrs packages [ "default" ]);

          devShells.default = craneLib.devShell {
            inherit (commonArgs) LD_LIBRARY_PATH;
            inputsFrom = [ releaseDeps ];
            packages = with pkgs; [
              rust-analyzer
              python3 # scripts/showcase.py
              # Nested sessions launch these; the DRM/udev backend links the rest.
              foot
              libinput
              udev
              libdrm
              libgbm
              seatd
            ];
          };

          formatter = pkgs.treefmt.withConfig {
            runtimeInputs = [
              pkgs.nixfmt
              pkgs.rustfmt
            ];
            settings = {
              tree-root-file = "flake.nix";
              formatter.nixfmt = {
                command = "nixfmt";
                includes = [ "*.nix" ];
              };
              formatter.rustfmt = {
                command = "rustfmt";
                options = [
                  "--edition"
                  "2024"
                ];
                includes = [ "*.rs" ];
              };
            };
          };
        };

      all = forAllSystems perSystem;
    in
    {
      packages = lib.mapAttrs (_: s: s.packages) all;
      checks = lib.mapAttrs (_: s: s.checks) all;
      devShells = lib.mapAttrs (_: s: s.devShells) all;
      formatter = lib.mapAttrs (_: s: s.formatter) all;
    };
}

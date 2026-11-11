{
  lib,
  rustPlatform,
  cmake,
  perl,
}:

rustPlatform.buildRustPackage {
  pname = "kasma";
  version = (lib.importTOML ./Cargo.toml).package.version;

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
    ];
  };

  cargoLock.lockFile = ./Cargo.lock;

  # aws-lc-sys (pulled in by rustls) builds C code with cmake.
  nativeBuildInputs = [
    cmake
    perl
  ];

  # cmake hook must not take over the cargo build.
  dontUseCmakeConfigure = true;

  meta = {
    description = "Multi-connection download accelerator";
    homepage = "https://github.com/BrisklyDev/kasma";
    license = lib.licenses.mit;
    mainProgram = "kasma";
    platforms = lib.platforms.unix;
  };
}

{
  lib,
  rustPlatform,
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

  meta = {
    description = "Multi-connection download accelerator";
    homepage = "https://github.com/BrisklyDev/kasma";
    license = lib.licenses.mit;
    mainProgram = "kasma";
    platforms = lib.platforms.unix;
  };
}

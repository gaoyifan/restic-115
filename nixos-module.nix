{self}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.restic-115;
  instanceModule = {
    options = {
      accessTokenFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "File containing the 115 access token, if no cached token is available.";
      };

      refreshTokenFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "File containing the 115 refresh token, if no cached token is available.";
      };

      cacheDirectory = lib.mkOption {
        type = lib.types.str;
        default = "/var/lib/restic-115";
        description = "Directory containing the restic-115 SQLite cache.";
      };

      repositoryPath = lib.mkOption {
        type = lib.types.str;
        default = "/restic-backup";
        description = "Root folder path on 115 for the repository.";
      };

      listenAddress = lib.mkOption {
        type = lib.types.str;
        default = "127.0.0.1";
        description = "Address on which restic-115 listens.";
      };

      listenPort = lib.mkOption {
        type = lib.types.port;
        default = 8000;
        description = "Port on which restic-115 listens.";
      };

      forceCacheRebuild = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Force rebuilding the restic-115 cache on startup.";
      };

      user = lib.mkOption {
        type = lib.types.str;
        default = "root";
        description = "User running restic-115.";
      };

      group = lib.mkOption {
        type = lib.types.str;
        default = "root";
        description = "Group running restic-115.";
      };
    };
  };
in {
  options.services.restic-115 = {
    enable = lib.mkEnableOption "restic-115 backend";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      description = "restic-115 package to run.";
    };

    instances = lib.mkOption {
      type = lib.types.attrsOf (lib.types.submodule instanceModule);
      default = {};
      description = "Named restic-115 backend instances.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.tmpfiles.rules =
      lib.mapAttrsToList (
        _: instance: "d ${instance.cacheDirectory} 0750 ${instance.user} ${instance.group} -"
      )
      cfg.instances;

    systemd.services =
      lib.mapAttrs' (
        name: instance:
          lib.nameValuePair "restic-115-${name}" {
            description = "Restic REST backend for 115 Open Platform storage (${name})";
            wantedBy = lib.mkDefault ["multi-user.target"];
            after = ["network-online.target"];
            wants = ["network-online.target"];
            unitConfig.RequiresMountsFor =
              [
                instance.cacheDirectory
              ]
              ++ lib.optional (instance.accessTokenFile != null) instance.accessTokenFile
              ++ lib.optional (instance.refreshTokenFile != null) instance.refreshTokenFile;
            serviceConfig = {
              ExecStart = lib.escapeShellArgs (
                [(lib.getExe cfg.package)]
                ++ lib.optionals (instance.accessTokenFile != null) [
                  "--access-token-file"
                  "%d/access-token"
                ]
                ++ lib.optionals (instance.refreshTokenFile != null) [
                  "--refresh-token-file"
                  "%d/refresh-token"
                ]
              );
              LoadCredential =
                lib.optional (instance.accessTokenFile != null) "access-token:${instance.accessTokenFile}"
                ++ lib.optional (instance.refreshTokenFile != null) "refresh-token:${instance.refreshTokenFile}";
              User = instance.user;
              Group = instance.group;
              Restart = "on-failure";
              RestartSec = "5s";
            };
            environment = {
              LISTEN_ADDR = instance.listenAddress;
              LISTEN_PORT = toString instance.listenPort;
              OPEN115_REPO_PATH = instance.repositoryPath;
              OPEN115_FORCE_CACHE_REBUILD = lib.boolToString instance.forceCacheRebuild;
              DB_PATH = "${instance.cacheDirectory}/cache-115.db";
            };
          }
      )
      cfg.instances;
  };
}

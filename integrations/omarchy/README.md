# Omarchy launcher

This optional bar widget opens the native Magpie desktop window. It uses the current desktop CLI's no-argument launch contract; the old draft's `app` and `panel` arguments are unsupported. The desktop layout is unchanged.

Build or install `magpie-desktop` first, then run this from an active Omarchy desktop session:

```sh
python3 integrations/omarchy/install.py install --executable /absolute/path/to/magpie-desktop
```

The helper creates `$XDG_CONFIG_HOME/omarchy/plugins/magpie-rs.magpie` (default: `~/.config/omarchy/plugins/magpie-rs.magpie`), rescans plugins and enables the launcher. It refuses an existing plugin directory, a symlink or a non-executable target. It does not change other plugins or restart the shell.

Remove this launcher with:

```sh
python3 integrations/omarchy/install.py remove
```

Removal checks the plugin identity, disables it, removes its directory and rescans. Repeating removal after the directory is gone succeeds without changing shell state.

## Validation

```sh
python3 integrations/omarchy/test_install.py
```

The tests use temporary configuration directories and stub shell commands to cover path escaping, the no-argument launch, refusal to overwrite, executable checks, rollback, scoped removal and symlink handling. They do not install anything in the current desktop session. The QML widget uses Omarchy’s `BarWidget` and `BarIconButton` interfaces. The installer tests do not exercise the running shell or render the desktop application.

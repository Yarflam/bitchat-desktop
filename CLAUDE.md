# Bitchat — notes pour l'agent

## Maintenance `desktop/` (workspace Rust)

Le dossier `desktop/target/debug` grossit sans limite (Cargo garde les anciens
artefacts hashés + la compilation incrémentale ; il est déjà atteint 17 GB).
Pendant les sessions de travail sur `desktop/`, penser à faire des passes de
nettoyage quand il dépasse ~5 GB :

```sh
cd desktop && cargo clean --profile dev
```

⚠️ Le profil s'appelle `dev` (pas `debug`). Le build suivant sera complet.

# Review: README Quick start als nieuwe gebruiker (`cargo install ci-bill`)

Read-only review: geen code aangepast, alleen dit bestand toegevoegd.

## Opzet

Simulatie van een nieuwe gebruiker die de README van boven naar beneden
volgt, startend met een gewone `cargo install ci-bill` (zonder repo-clone,
zonder bestaande `~/.cargo`-cache). Omgeving: Linux x86_64, `cargo`/`rustc`
1.97.0.

## Samenvatting

Installatie zelf werkt probleemloos. Het probleem zit in het enige
commando dat de README aanreikt om de tool na installatie uit te proberen:
de `## Example`-sectie. Dat commando gaat ervan uit dat je in een git-clone
van de repo zit (`cargo run` + het relatieve pad `samples/...`), terwijl de
`## Installation`-sectie er vlak boven juist `cargo install` beschrijft,
wat geen source-checkout achterlaat. Een gebruiker die het Quick start-pad
letterlijk volgt (install → example) loopt vast op de enige "probeer het
uit"-stap in de hele README.

## Reproductie

1. `cargo install ci-bill` in een lege `CARGO_HOME`, geen repo-clone erbij:

   ```
   Installed package `ci-bill v0.1.1` (executable `ci-bill`)
   ```

   Geen warnings. `ci-bill --help` en `ci-bill prices` werken meteen (die
   laatste zonder bestanden of netwerk nodig te hebben).

2. De `## Example`-sectie letterlijk uitgevoerd, in de working directory
   van diezelfde kersverse gebruiker (geen clone aanwezig):

   ```sh
   cargo run --quiet -- report --usage samples/usage-sample.csv --workflows samples/.github/workflows
   ```

   Resultaat:

   ```
   error: could not find `Cargo.toml` in `<cwd>` or any parent directory
   ```

   Zelfs mét een `Cargo.toml` in de buurt zou dit nog stuklopen op
   `samples/usage-sample.csv: cannot read file` (exit 2): `samples/` bestaat
   alleen in de git-checkout (en, als implementatiedetail, in cargo's eigen
   registry-cache onder `~/.cargo/registry/src/...`), niet naast het
   geïnstalleerde binary.

3. Ter controle: hetzelfde commando in een echte clone van de repo geeft
   wél exact de output die de README toont (1-op-1 geverifieerd, geen
   documentatiedrift in de voorbeeldoutput zelf). Het voorbeeld klopt dus
   inhoudelijk — het is alleen onbereikbaar vanaf het `cargo install`-pad
   dat er in dezelfde README toe aanzet.

## Impact

- Dit is de enige stap in de README waarmee een nieuwe gebruiker de tool
  met voorbeelddata kan zien draaien. Na een geadviseerde `cargo install
  ci-bill` is er geen enkel werkend copy-paste commando om dat te doen.
- `ci-bill prices` werkt wél meteen (geen bestanden, geen netwerk) en zou
  een prima eerste smoke-test zijn, maar wordt nergens als zodanig
  aangewezen.

## Suggesties (niet doorgevoerd — read-only review)

- Zet in `## Example` een expliciete `git clone`/`cd`-stap voor wie net
  `cargo install` deed, of geef een manier om alleen `samples/` op te
  halen zonder de hele repo te clonen.
- Of noem in `## Installation` expliciet dat `ci-bill prices` de kortste
  eerste smoke-test is direct na `cargo install`, zodat er wél een
  bereikbare "probeer het uit"-stap is voor wie niet gaat clonen.

## Wel in orde

- `cargo install ci-bill` (zonder `--locked`) compileert schoon, geen
  warnings.
- Geen systeem-OpenSSL nodig dankzij de `rustls`-feature van `ureq`, dus
  geen extra systeempakketten voor de install zelf.
- `ci-bill --help`, `ci-bill report --help` en `ci-bill prices` zijn
  correct en consistent met wat de README belooft.
- Alleen getest op Linux x86_64; de macOS- en Windows-installatiepaden
  (archieven, `cargo install` aldaar) zijn niet apart geverifieerd.

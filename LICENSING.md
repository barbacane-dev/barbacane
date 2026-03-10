# Licensing

Barbacane is dual-licensed:

| License | When it applies |
|---------|----------------|
| [GNU Affero General Public License v3](LICENSE) (AGPLv3) | Default for everyone |
| [Commercial License](LICENSE-COMMERCIAL) | When you cannot or do not want to comply with AGPLv3 |

---

## Which license do I need?

### You can use the AGPLv3 if…

- You use Barbacane internally and never expose it as a service to third parties.
- You run it as a public service **and** you publish the complete corresponding source code of your modified version under the AGPLv3.
- You are building open-source software that is itself released under an AGPLv3-compatible license.

The AGPLv3 is a copyleft license: if you run a modified version of Barbacane over a network, you must make your modifications available to users under the same license.

### You need a Commercial License if…

- You want to offer Barbacane (or a product built on it) as a hosted/managed service without publishing your source modifications.
- You cannot comply with the copyleft requirements of the AGPLv3 for business or legal reasons.

---

## Commercial License — Free Tier

A commercial license is **free of charge** for:

- **Small startups** — annual revenue (ARR) ≤ €1 M *and* ≤ 10 employees.
- **Non-profits, academic institutions, and open-source projects** whose output is itself open-source.

See [LICENSE-COMMERCIAL](LICENSE-COMMERCIAL) for the full terms.

---

## Commercial License — Paid Tier

Larger organisations that do not qualify for the Free Tier must purchase a
commercial license.  Pricing and enterprise agreements are available at
**https://barbacane.dev/pricing** or by emailing **contact@barbacane.dev**.

---

## OpenSSL Exception

Barbacane links against `aws-lc-rs` (via `rustls`) for cryptographic
operations.  The AGPLv3 text in this repository includes an additional
permission (OpenSSL exception) that explicitly allows this combination and
distribution of the resulting binaries.  See the end of [LICENSE](LICENSE)
for the exact wording.

---

## Questions?

If you are unsure which license applies to your use case, reach out at
**contact@barbacane.dev** — we are happy to help.

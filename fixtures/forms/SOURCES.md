# Fixture forms

Blank, official public-sector forms with no field values, used as the fill
corpus. They are not byte-for-byte as issued: document-level metadata naming
agency staff (Info dictionary Author/Creator/Producer/Company and the XMP
metadata stream; on the buyer's guide also the Illustrator art's embedded
XMP and a workstation temp path in the XFA config packet) was stripped and
each file was re-saved. The MV-1, T-7 and T-8 were decrypted, cleaned and
re-encrypted with qpdf `--copy-encryption` from the issued file, so the
encryption dictionary (algorithm, R, V, P, O/U entries and the issuer's owner
password) and the document /ID are unchanged and they still open with an
empty user password. The GA-25 was patched in place at the same length so its
stale linearization hints are kept. Page content, fields and form structure
are untouched: same page and field counts, all fields still empty, and every
page renders pixel-identical to the issued file at 72 dpi. Each was opened,
rendered and its fields dumped before inclusion.
They are government works distributed for public use. Get current versions from
the issuing agency before filing anything.

| File | Form | Issuer | Notes |
| --- | --- | --- | --- |
| `ga-mv1-title-application.pdf` | MV-1 Motor Vehicle Title Application (rev. 6-2020) | Georgia Department of Revenue, Motor Vehicle Division | 2 pages, 112 fields; AES encrypted with an owner password (no user password), fill-forms permission set |
| `ga-t7-bill-of-sale.pdf` | T-7 Bill of Sale (rev. 9-2022) | Georgia Department of Revenue, Motor Vehicle Division | 1 page, 48 fields; AES-256 owner-password encryption; 17-cell comb VIN field |
| `ga-t8-limited-power-of-attorney.pdf` | T-8 Limited Power of Attorney (rev. 3-2015) | Georgia Department of Revenue, Motor Vehicle Division | 1 page, 24 fields; owner-password encryption |
| `ftc-buyers-guide.pdf` | Used Vehicle Buyers Guide (16 CFR 455), English | U.S. Federal Trade Commission | 3 pages, 43 fields; hybrid XFA + AcroForm (LiveCycle Designer), carries Adobe's standard reader-version-check JavaScript |
| `ga25-odometer-disclosure.pdf` | Form GA-25 Odometer Disclosure Statement (non-leased vehicles) | Georgia | 1 page, flat (no AcroForm): exercises the no-fields path and qpdf warning handling |

SHA-256:

```
7bf8fc4c4cce313deb39c2501aed33f93da600f27c4fa67381e45d1860cc8cd1  ftc-buyers-guide.pdf
be761631d36d24834a06240183c944568c04eea05cece8d3ef57e0510f99bbbe  ga-mv1-title-application.pdf
76c8e67fb611b6fe348dcd09ed3d7b7b335a1fc8e5aed100074c35d7813fa962  ga-t7-bill-of-sale.pdf
5ea2ff153d7898289a6fa73c00dfad725a7815cfe5fb6f620faa7f65cc58caa1  ga-t8-limited-power-of-attorney.pdf
4bad3646f5343bf02c834bf2fa23b76f2b6ebecaf6263041143e49ea3251e202  ga25-odometer-disclosure.pdf
```

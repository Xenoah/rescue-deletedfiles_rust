# OpenDocument and EPUB fixtures

The manifest records the generator versions, stored media type, entry count and SHA-256 for each original. odfpy wrote and reopened the 17 OpenDocument variants. The text and spreadsheet fixtures contain Japanese text. The formula packages contain MathML. Pandoc produced an additional ODT and two EPUB books from Markdown. EbookLib produced the third EPUB and reopened the three books. All test documents were generated for this repository.

Run `python scripts/e2e_open_documents.py --binary /path/to/rescue` to scan and recover the originals in regular and indexed modes. Pass `--previous /path/to/older/rescue` to verify that generic ZIP sessions made by an earlier release still recover the same bytes.

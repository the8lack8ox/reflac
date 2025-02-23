# reflac

Easy tagging of FLAC audio files

## Usage

```bash
reflac "path to TRACKINFO file" ["optional output location"]
```

reflac relies on TRACKINFO files that describe a complete album.

Track info files look something like ...

```text
INPUT=path/to/input/files
ALBUM=Album Name
ARTIST=Artist Name
GENRE=Rock
DATE=YYYY-MM-dd
COVER=cover.jpg
TITLE[1]=First track name
TITLE[2]=Second track name
TITLE[3]=Third track name
```

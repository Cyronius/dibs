import os, sys

if sys.version_info < (3, 11):
    sys.stderr.write(f"dibs needs Python 3.11 or newer; {sys.executable} is {sys.version.split()[0]}.\n")
    sys.exit(97)
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from dibs.cli import main  # noqa: E402

sys.exit(main())

"""Frozen Windows entry point for TAPE//BREAK."""
import sys
from vhsgui.__main__ import main

if __name__ == "__main__":
    # The released product opens in its simplified editing workspace.
    arguments = list(sys.argv[1:])
    if "--schematic" not in arguments and "--panels" not in arguments:
        arguments.insert(0, "--panels")
    sys.exit(main(arguments))

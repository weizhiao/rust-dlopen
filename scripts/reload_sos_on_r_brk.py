from gdb import (
    NewObjFileEvent,
    Value,
    execute,
    objfiles,
    BP_BREAKPOINT,
    Breakpoint,
    parse_and_eval,
    events,
)
from elftools.elf.elffile import ELFFile

# Global flag to track whether the breakpoint has been set
BRK_INIT = False


def try_manually_adding_symbols(lib_name, l_addr):
    """Attempts to manually load debug symbols for a shared library.

    This function reads the ELF file to locate the `.text` section and
    calculates its virtual memory address (VMA). It then instructs GDB
    to add symbol information based on the loaded address.

    Args:
        lib_name (str): Path to the shared library.
        l_addr (int): Base address where the library is loaded.
    """
    try:
        with open(lib_name, "rb") as elf_file:
            elf = ELFFile(elf_file)
            text_section = elf.get_section_by_name(".text")
            if text_section is None:
                return
            text_section_vma = text_section["sh_addr"]
            command = f"add-symbol-file {lib_name} {l_addr + text_section_vma}"
            execute(command)
    except Exception:
        pass


def ensure_shared_library(lib_path: str, lib_addr: int):
    """Ensures that the specified shared library has its symbols loaded.

    This function checks whether the shared object (SO) file has already been
    loaded in GDB. It does so by comparing only the file names (not full paths)
    to avoid false positives. If the library isn't recognized, it attempts to
    manually add its symbols.

    Args:
        lib_path (str): Full path to the shared library.
        lib_addr (int): Base address where the library is loaded.
    """
    loaded_lib_names = list(
        set(
            so.filename.split("/")[-1]
            for so in objfiles()[1:]  # Skip the main executable
            if so.filename is not None and " " not in so.filename
        )
    )
    lib_name = lib_path.split("/")[-1]

    if lib_name not in loaded_lib_names:
        try_manually_adding_symbols(lib_path, lib_addr)


class BrkReloadAllSharedLib(Breakpoint):
    """Breakpoint that ensures symbols are loaded for all shared libraries.

    This breakpoint is placed at `_r_debug.r_brk`, which is triggered when
    shared libraries are loaded or unloaded. It iterates over the linked list
    of loaded libraries (`r_map`), checking for any that are missing symbols
    and loading them if necessary.
    """

    def __init__(self, debug: Value, brk: Value):
        super().__init__(f"*{int(brk)}", BP_BREAKPOINT, internal=False)
        self.debug = debug

    def stop(self) -> bool:
        """Handles the breakpoint hit by processing the shared library list.

        This function traverses the `r_map` linked list to identify newly
        loaded shared libraries that may be missing symbols. It then loads
        the missing symbols into GDB.

        Returns:
            bool: Always returns `False` to continue execution after the breakpoint.
        """
        link = self.debug.dereference()["r_map"]

        # Traverse the linked list of loaded shared objects
        while link != 0:
            # Extract and clean up the library path from the C string
            lib_path = str(link.dereference()["l_name"]).split(" ")[1][1:-1].strip()
            if lib_path == "" or lib_path == "linux-vdso.so.1":
                link = link.dereference()["l_next"]
                continue

            # Get the memory address where the shared library is loaded
            l_addr = int(link.dereference()["l_addr"])

            ensure_shared_library(lib_path, l_addr)
            link = link.dereference()["l_next"]

        return False


def object_event_hook(event: NewObjFileEvent) -> object:
    """Handles new shared object file events in GDB.

    When a new shared object file is loaded, this function checks if `_r_debug`
    is properly set up and places a breakpoint at `_r_debug.r_brk` if needed.

    Args:
        event (NewObjFileEvent): GDB event triggered by loading a new object file.
    """
    global BRK_INIT
    debug = parse_and_eval("(struct r_debug *) &_r_debug")
    r_brk = debug.dereference()["r_brk"]

    # Ensure `_r_debug.r_brk` is set before placing the breakpoint
    if not BRK_INIT and r_brk != 0:
        BrkReloadAllSharedLib(debug, r_brk)
        BRK_INIT = True


# Register the event hook to track shared library loading.
# The `_r_debug` structure is not exposed initially in GDB, but can be accessed
# once the program is in the correct execution state. This hook ensures that
# when a new shared object is loaded, we properly set up the breakpoint to handle it.
events.new_objfile.connect(object_event_hook)

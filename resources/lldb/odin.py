# Original author: laytan (https://gist.github.com/laytan/a94c323a84cef7bcfbdf6d21987fd5a9)
# Modifications by: harold-b (https://gist.github.com/harold-b/ef16a5c3ebcceccfc2bc7a5c5dd0058d)
# Modifications by: NANDquark

import logging
import math

import lldb

log = logging.getLogger(__name__)


def is_slice_type(t, internal_dict):
    name = t.name or ""
    return (
        name.startswith("[]")  # slice
        or name.startswith("[dynamic]")  # dynamic array
        or name.startswith("[dynamic;")  # fixed capacity dynamic array
    ) and not name.endswith("]")


def slice_summary(value, internal_dict):
    value = value.GetNonSyntheticValue()
    type_name = value.GetType().GetDisplayTypeName()

    if type_name.startswith("[]"):  # slice
        len = value.GetChildMemberWithName("len").unsigned
        return f"{type_name} (len:{len})"
    elif type_name.startswith("[dynamic]"):  # dynamic array
        len = value.GetChildMemberWithName("len").unsigned
        cap = value.GetChildMemberWithName("cap").unsigned
        return f"{type_name} (len:{len}, cap:{cap})"
    elif type_name.startswith("[dynamic;"):  # fixed capacity dynamic array
        len_field = value.GetChildMemberWithName("len")
        if len_field.IsValid():
            len = len_field.unsigned
            return f"{type_name} (len:{len})"

    return type_name


class SliceChildProvider:
    CHUNK_COUNT = 2000

    def __init__(self, val, dict):
        self.val = val
        self.update()

    def update(self):
        val = self.val

        self.len = val.GetChildMemberWithName("len").unsigned
        self.data_val = val.GetChildMemberWithName("data")
        assert self.data_val.type.is_pointer

        is_chunked = self.len > SliceChildProvider.CHUNK_COUNT
        self.chunked_len = (
            0
            if not is_chunked
            else math.ceil(self.len / SliceChildProvider.CHUNK_COUNT)
        )

        return False

    def num_children(self):
        return self.chunked_len if self.chunked_len > 0 else self.len

    def get_child_at_index(self, index):
        length = self.num_children()
        assert index >= 0 and index < length

        first = self.data_val.deref

        if self.chunked_len > 0:
            chunk_size = SliceChildProvider.CHUNK_COUNT

            array_len = min(chunk_size, self.len - index * chunk_size)
            arr_type = first.type.GetArrayType(array_len)
            offset = index * first.size * chunk_size

            range_start = index * chunk_size

            return self.data_val.CreateChildAtOffset(
                f"[{range_start}..<{range_start + array_len}]", offset, arr_type
            )

        offset = index * first.size
        return self.data_val.CreateChildAtOffset(f"[{index}]", offset, first.type)


def is_string_type(t, internal_dict):
    return t.name == "string"


def string_summary(value, internal_dict):
    pointer = value.GetChildMemberWithName("data").GetValueAsUnsigned(0)
    length = value.GetChildMemberWithName("len").GetValueAsSigned(0)
    if pointer == 0:
        return False
    if length == 0:
        return '""'
    error = lldb.SBError()
    string_data = value.process.ReadMemory(pointer, length, error)
    return '"{}"'.format(string_data.decode("utf-8"))


def is_map_type(t, internal_dict):
    return t.name.startswith("map[")


def map_summary(value, internal_dict):
    value = value.GetNonSyntheticValue()
    type_name = value.GetType().GetDisplayTypeName()
    len_field = value.GetChildMemberWithName("len")

    if not len_field.IsValid():
        return type_name

    length = len_field.GetValueAsUnsigned()
    return f"{type_name} (len:{length})"


class MapChildProvider:
    TOMBSTONE_MASK_64 = 1 << 63
    MAX_KEY_LABEL_DEPTH = 2
    MAX_KEY_LABEL_CHILDREN = 8
    MAX_KEY_LABEL_LENGTH = 160

    def __init__(self, val, internal_dict):
        self.val = val
        self.raw = val.GetNonSyntheticValue()
        self.entries = []
        self.len_type = None
        self.cap = 0

    def update(self):
        self.raw = self.val.GetNonSyntheticValue()
        self.entries = []

        data = self.raw.GetChildMemberWithName("data")
        len_val = self.raw.GetChildMemberWithName("len")

        if not data.IsValid() or not len_val.IsValid():
            self.cap = 0
            self.len_type = None
            return True

        self.len_type = len_val.GetType()

        tkey = data.GetChildMemberWithName("key").GetType()
        tval = data.GetChildMemberWithName("value").GetType()
        hash_field = data.GetChildMemberWithName("hash")
        key_cell = data.GetChildMemberWithName("key_cell")
        value_cell = data.GetChildMemberWithName("value_cell")

        if (
            not tkey.IsValid()
            or not tval.IsValid()
            or not hash_field.IsValid()
            or not key_cell.IsValid()
            or not value_cell.IsValid()
        ):
            self.cap = 0
            return True

        raw_data = data.GetValueAsUnsigned()
        key_ptr = raw_data & ~63
        cap_log2 = raw_data & 63
        self.cap = 0 if cap_log2 <= 0 else 1 << cap_log2

        if self.cap == 0:
            return True

        key_cell_info = self.cell_info(tkey, key_cell)
        value_cell_info = self.cell_info(tval, value_cell)

        size_of_hash = hash_field.GetByteSize()
        if size_of_hash != 8:
            return True

        value_ptr = self.cell_index(key_ptr, key_cell_info, self.cap)
        hash_ptr = self.cell_index(value_ptr, value_cell_info, self.cap)

        process = self.val.GetProcess()
        error = lldb.SBError()

        live_index = 0
        for i in range(self.cap):
            offset_hash = hash_ptr + i * size_of_hash
            hash_val = process.ReadUnsignedFromMemory(offset_hash, size_of_hash, error)
            if not error.Success():
                error.Clear()
                continue

            if hash_val == 0 or (hash_val & self.TOMBSTONE_MASK_64) != 0:
                continue

            offset_key = self.cell_index(key_ptr, key_cell_info, i)
            offset_value = self.cell_index(value_ptr, value_cell_info, i)

            key_val = self.val.CreateValueFromAddress(
                f"key[{live_index}]", offset_key, tkey
            )
            value_val = self.val.CreateValueFromAddress(
                f"value[{live_index}]", offset_value, tval
            )

            if key_val.IsValid() and value_val.IsValid():
                self.entries.append((key_val, value_val))

                live_index += 1

        return True

    def has_children(self):
        return True

    def num_children(self):
        return len(self.entries) + 1  # +1 for cap

    def get_child_index(self, name):
        if name == "cap":
            return len(self.entries)

        # match the exact synthetic names we generate below
        for i, (key_val, _value_val) in enumerate(self.entries):
            if self.entry_name(key_val) == name:
                return i

        return -1

    def get_child_at_index(self, index):
        if index < 0:
            return lldb.SBValue()

        if index == len(self.entries):
            if self.len_type is None:
                return lldb.SBValue()
            cap_data = lldb.SBData.CreateDataFromInt(self.cap)
            return self.val.CreateValueFromData("cap", cap_data, self.len_type)

        if index >= len(self.entries):
            return lldb.SBValue()

        key_val, value_val = self.entries[index]
        return value_val.Clone(self.entry_name(key_val))

    def entry_name(self, key_val):
        key_label = self.key_label(key_val)
        if key_label:
            return f"[{self.truncate_key_label(key_label)}]"
        return "[unrecognized-entry]"

    def key_label(self, val, depth=0):
        if not val.IsValid():
            return None

        val = val.GetNonSyntheticValue()
        val_type = val.GetType()
        child_count = val.GetNumChildren()

        if (
            child_count > 0
            and not self.is_string_key_type(val_type)
            and not getattr(val_type, "is_pointer", False)
        ):
            return self.aggregate_key_label(val, child_count, depth)

        summary = val.GetSummary()
        if summary:
            return summary

        value = val.GetValue()
        if value:
            return value

        obj_desc = val.GetObjectDescription()
        if obj_desc:
            return obj_desc

        return None

    def is_string_key_type(self, val_type):
        if val_type.GetDisplayTypeName() == "string":
            return True

        get_canonical_type = getattr(val_type, "GetCanonicalType", None)
        if not get_canonical_type:
            return False

        canonical_type = get_canonical_type()
        return (
            canonical_type.IsValid() and canonical_type.GetDisplayTypeName() == "string"
        )

    def aggregate_key_label(self, val, child_count, depth):
        if depth >= self.MAX_KEY_LABEL_DEPTH:
            return "{...}"

        fields = []
        shown_children = min(child_count, self.MAX_KEY_LABEL_CHILDREN)

        for i in range(shown_children):
            child = val.GetChildAtIndex(i).GetNonSyntheticValue()
            child_name = child.GetName() or f"#{i}"
            child_label = self.key_label(child, depth + 1) or "?"
            fields.append(f"{child_name}:{child_label}")

        if child_count > shown_children:
            fields.append("...")

        return "{" + ", ".join(fields) + "}"

    def truncate_key_label(self, label):
        if len(label) <= self.MAX_KEY_LABEL_LENGTH:
            return label
        return label[: self.MAX_KEY_LABEL_LENGTH - 3] + "..."

    def cell_info(self, typev, cell_type):
        type_size = typev.GetByteSize()
        cell_size = cell_type.GetByteSize()
        elements_per_cell = 0

        if type_size != cell_size:
            first_child = cell_type.GetChildAtIndex(0)
            if first_child.IsValid():
                array_type = first_child.GetType()
                array_size = array_type.GetByteSize()
                if array_size > 0 and type_size > 0:
                    elements_per_cell = array_size // type_size

        if elements_per_cell == 0:
            elements_per_cell = 1

        return CellInfo(type_size, cell_size, elements_per_cell)

    def cell_index(self, base, info, index):
        if info.elements_per_cell == 1:
            return base + (index * info.size_of_cell)
        elif info.elements_per_cell == 2:
            cell_index = index >> 1
            data_index = index & 1
        elif info.elements_per_cell == 4:
            cell_index = index >> 2
            data_index = index & 3
        elif info.elements_per_cell == 8:
            cell_index = index >> 3
            data_index = index & 7
        elif info.elements_per_cell == 16:
            cell_index = index >> 4
            data_index = index & 15
        elif info.elements_per_cell == 32:
            cell_index = index >> 5
            data_index = index & 31
        else:
            cell_index = index // info.elements_per_cell
            data_index = index % info.elements_per_cell

        return (
            base + (cell_index * info.size_of_cell) + (data_index * info.size_of_type)
        )


class _MapChildProvider:
    def __init__(self, val, dict):
        self.val = val

    def num_children(self):
        return (self.val.GetChildMemberWithName("len").GetValueAsSigned() * 2) + 1

    def get_child_at_index(self, index):
        data = self.val.GetChildMemberWithName("data")
        tkey = data.GetChildMemberWithName("key").type
        tval = data.GetChildMemberWithName("value").type
        hash_field = data.GetChildMemberWithName("hash")
        key_cell = data.GetChildMemberWithName("key_cell")
        value_cell = data.GetChildMemberWithName("value_cell")

        raw_data = data.GetValueAsUnsigned()
        key_ptr = raw_data & ~63
        cap_log2 = raw_data & 63
        cap = 0 if cap_log2 <= 0 else 1 << cap_log2

        key_cell_info = self.cell_info(tkey, key_cell)
        value_cell_info = self.cell_info(tval, value_cell)

        size_of_hash = hash_field.size
        assert size_of_hash == 8

        value_ptr = self.cell_index(key_ptr, key_cell_info, cap)
        hash_ptr = self.cell_index(value_ptr, value_cell_info, cap)

        error = lldb.SBError()

        # Last one, the capacity.
        if index == self.num_children() - 1:
            cap_data = lldb.SBData.CreateDataFromInt(cap)
            return self.val.CreateValueFromData(
                "cap", cap_data, self.val.GetChildMemberWithName("len").type
            )

        wants_key = index % 2 == 0
        index = int(index / 2)

        key_index = 0
        for i in range(cap):
            TOMBSTONE_MASK = 1 << (size_of_hash * 8 - 1)

            offset_hash = hash_ptr + i * size_of_hash

            hash_val = self.val.process.ReadUnsignedFromMemory(
                offset_hash, size_of_hash, error
            )
            if not error.success:
                print(error)
                continue
            elif hash_val == 0 or (hash_val & TOMBSTONE_MASK) != 0:
                continue

            offset_key = self.cell_index(key_ptr, key_cell_info, i)
            offset_value = self.cell_index(value_ptr, value_cell_info, i)

            if index == key_index:
                if wants_key:
                    return self.val.CreateValueFromAddress(f"[{i}]", offset_key, tkey)
                else:
                    return self.val.CreateValueFromAddress(f"[{i}]", offset_value, tval)

            key_index += 1

        print("not found")

    def cell_info(self, typev, cell_type):
        elements_per_cell = 0

        if typev.size != cell_type.size:
            array_type = cell_type.children[0].type
            if array_type.size > 0 and typev.size > 0:
                elements_per_cell = array_type.size / typev.size

        if elements_per_cell == 0:
            elements_per_cell = 1

        return CellInfo(typev.size, cell_type.size, elements_per_cell)

    def cell_index(self, base, info, index):
        cell_index = 0
        data_index = 0
        if info.elements_per_cell == 1:
            return base + (index * info.size_of_cell)
        elif info.elements_per_cell == 2:
            cell_index = index >> 1
            data_index = index & 1
        elif info.elements_per_cell == 4:
            cell_index = index >> 2
            data_index = index & 3
        elif info.elements_per_cell == 8:
            cell_index = index >> 3
            data_index = index & 7
        elif info.elements_per_cell == 16:
            cell_index = index >> 4
            data_index = index & 15
        elif info.elements_per_cell == 32:
            cell_index = index >> 5
            data_index = index & 31
        else:
            cell_index = index / info.elements_per_cell
            data_index = index % info.elements_per_cell

        return (
            base + (cell_index * info.size_of_cell) + (data_index * info.size_of_type)
        )


class CellInfo:
    def __init__(self, size_of_type, size_of_cell, elements_per_cell):
        self.size_of_type = size_of_type
        self.size_of_cell = size_of_cell
        self.elements_per_cell = elements_per_cell


class UnionChildProvider:
    def __init__(self, val, dict):
        self.val = val

    def update(self):
        self.children = self.val.children
        self.variant_index = self.children[0].unsigned
        return False

    def num_children(self):
        return len(self.children) - 1

    def get_child_at_index(self, index):
        value = self.val

        variant_index = index + 1
        variant = self.children[variant_index]
        name = variant.type.GetDisplayTypeName()
        # offset        = variant.addr.offset - value.addr.offset
        selected = "*" if self.variant_index == variant_index else ""

        field_name = f"{selected}v{variant_index}({name})"
        # c = value.CreateChildAtOffset( field_name, offset, variant.GetType() )
        c = value.CreateValueFromData(field_name, variant.data, variant.type)

        return c


def is_type_union(t, internal_dict):
    if t.type != lldb.eTypeClassUnion:
        return False

    tag = t.GetFieldAtIndex(0)
    return tag and tag.IsValid() and tag.name == "tag"


def union_summary(value, internal_dict):
    if value.IsSynthetic():
        value = value.GetNonSyntheticValue()

    tag = value.GetChildAtIndex(0)
    assert tag.name == "tag"
    # tag = value.GetChildMemberWithName("tag")

    variant_name = f"v{tag.unsigned}"
    variant = value.GetChildMemberWithName(variant_name)
    # variant_type = variant.type.GetDisplayTypeName()

    return f"{variant}"


def __lldb_init_module(debugger, unused):
    debugger.HandleCommand(
        "type summary add --recognizer-function --python-function odin.union_summary odin.is_type_union"
    )
    debugger.HandleCommand(
        "type synth add --recognizer-function --python-class odin.UnionChildProvider odin.is_type_union"
    )

    debugger.HandleCommand(
        "type summary add --recognizer-function --python-function odin.string_summary odin.is_string_type"
    )

    debugger.HandleCommand(
        "type synth add --recognizer-function --python-class odin.SliceChildProvider odin.is_slice_type"
    )
    debugger.HandleCommand(
        "type summary add --recognizer-function --python-function odin.slice_summary odin.is_slice_type"
    )

    debugger.HandleCommand(
        "type synth add --recognizer-function --python-class odin.MapChildProvider odin.is_map_type"
    )
    debugger.HandleCommand(
        "type summary add --recognizer-function --python-function odin.map_summary odin.is_map_type"
    )

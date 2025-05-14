-- MetalBond Protocol Dissector for Wireshark with custom header support
-- Based on proto/metalbond.proto with header framing and TCP stream reassembly support
--
-- MetalBond message format:
-- Every MetalBond protobuf message is prefixed with a custom header:
-- 8 bits version (must be 1)
-- 16 bits message length of the following protobuf message
-- 8 bits message type of the following protobuf message 
--
-- Message Types:
-- 1  HELLO
-- 2  KEEPALIVE
-- 3  SUBSCRIBE
-- 4  UNSUBSCRIBE
-- 5  UPDATE

-- Protocol definition with unique name
local metalbond_proto = Proto("metalbond", "MetalBond Protocol")

-- Expert info fields
local expert = {
    note = ProtoExpert.new("metalbond.note", "For complete decoding, use the Protobuf dissector with metalbond.proto", 
                           PI_UNDECODED, PI_NOTE),
    update_note = ProtoExpert.new("metalbond.update_note", "Update message detected - use protobuf dissector for full decoding", 
                                  PI_UNDECODED, PI_NOTE),
    malformed = ProtoExpert.new("metalbond.malformed", "Invalid protobuf data", 
                                PI_MALFORMED, PI_ERROR),
    undecoded = ProtoExpert.new("metalbond.undecoded", "Could not determine message type", 
                                PI_UNDECODED, PI_WARN)
}

-- Register expert info
metalbond_proto.experts = expert

-- Protocol fields
local fields = metalbond_proto.fields

-- Header fields
fields.header_version = ProtoField.uint8("metalbond.header.version", "Version", base.DEC)
fields.header_msg_length = ProtoField.uint16("metalbond.header.msg_length", "Message Length", base.DEC)
fields.header_msg_type = ProtoField.uint8("metalbond.header.msg_type", "Message Type", base.DEC, {
    [1] = "HELLO",
    [2] = "KEEPALIVE",
    [3] = "SUBSCRIBE",
    [4] = "UNSUBSCRIBE",
    [5] = "UPDATE"
})

-- Hello message fields
fields.hello_keepalive_interval = ProtoField.uint32("metalbond.hello.keepalive_interval", "Keepalive Interval", base.DEC)
fields.hello_is_server = ProtoField.bool("metalbond.hello.is_server", "Is Server")

-- Subscription message fields
fields.subscription_vni = ProtoField.uint32("metalbond.subscription.vni", "VNI", base.DEC)

-- Update message fields
fields.update_action = ProtoField.uint32("metalbond.update.action", "Action", base.DEC, {
    [0] = "ADD",
    [1] = "REMOVE"
})
fields.update_vni = ProtoField.uint32("metalbond.update.vni", "VNI", base.DEC)

-- Destination fields
fields.destination_ip_version = ProtoField.uint32("metalbond.destination.ip_version", "IP Version", base.DEC, {
    [0] = "IPv4",
    [1] = "IPv6"
})
fields.destination_prefix = ProtoField.bytes("metalbond.destination.prefix", "Prefix")
fields.destination_prefix_length = ProtoField.uint32("metalbond.destination.prefix_length", "Prefix Length", base.DEC)

-- NextHop fields
fields.next_hop_target_address = ProtoField.bytes("metalbond.next_hop.target_address", "Target Address")
fields.next_hop_target_vni = ProtoField.uint32("metalbond.next_hop.target_vni", "Target VNI", base.DEC)
fields.next_hop_type = ProtoField.uint32("metalbond.next_hop.type", "Type", base.DEC, {
    [0] = "STANDARD",
    [1] = "NAT",
    [2] = "LOADBALANCER_TARGET"
})
fields.next_hop_nat_port_range_from = ProtoField.uint32("metalbond.next_hop.nat_port_range_from", "NAT Port Range From", base.DEC)
fields.next_hop_nat_port_range_to = ProtoField.uint32("metalbond.next_hop.nat_port_range_to", "NAT Port Range To", base.DEC)

-- Utility: Convert varint-encoded value to number (for protobuf parsing)
local function decode_varint(buffer, offset)
    local value = 0
    local shift = 0
    local index = offset
    local byte

    repeat
        if index >= buffer:len() then
            return nil, index - offset -- Could not decode complete varint
        end
        
        byte = buffer(index, 1):uint()
        value = value + bit.band(byte, 0x7F) * bit.lshift(1, shift)
        shift = shift + 7
        index = index + 1
    until bit.band(byte, 0x80) == 0

    return value, index - offset
end

-- Utility: Parse field key (for protobuf parsing)
local function parse_key(buffer, offset)
    local key, bytes_consumed = decode_varint(buffer, offset)
    if not key then return nil, nil, bytes_consumed end
    
    local field_number = bit.rshift(key, 3)
    local wire_type = bit.band(key, 0x07)
    
    return field_number, wire_type, bytes_consumed
end

-- Utility: Format IP address from bytes
local function format_ip(buf, ip_version)
    if ip_version == 0 then -- IPv4
        if buf:len() >= 4 then
            return string.format("%d.%d.%d.%d", buf(0,1):uint(), buf(1,1):uint(), buf(2,1):uint(), buf(3,1):uint())
        else
            return "Invalid IPv4 address (insufficient bytes)"
        end
    elseif ip_version == 1 then -- IPv6
        if buf:len() >= 16 then
            local parts = {}
            for i = 0, 7 do
                local val = buf(i*2,1):uint() * 256 + buf(i*2+1,1):uint()
                parts[i+1] = string.format("%04x", val)
            end
            return table.concat(parts, ":")
        else
            return "Invalid IPv6 address (insufficient bytes)"
        end
    else
        return "Unknown IP version"
    end
end

-- Protobuf field parsers
local function parse_hello(buffer, subtree, offset)
    local current_offset = offset
    local field_num, wire_type, consumed
    
    while current_offset < buffer:len() do
        field_num, wire_type, consumed = parse_key(buffer, current_offset)
        if not field_num then break end
        current_offset = current_offset + consumed
        
        if field_num == 1 and wire_type == 0 then -- keepaliveInterval (varint)
            local value, consumed = decode_varint(buffer, current_offset)
            if value then
                subtree:add(fields.hello_keepalive_interval, value)
                current_offset = current_offset + consumed
            end
        elseif field_num == 2 and wire_type == 0 then -- isServer (bool/varint)
            local value, consumed = decode_varint(buffer, current_offset)
            if value then
                subtree:add(fields.hello_is_server, value ~= 0)
                current_offset = current_offset + consumed
            end
        else
            -- Skip unknown field
            if wire_type == 0 then -- varint
                local _, consumed = decode_varint(buffer, current_offset)
                if consumed then
                    current_offset = current_offset + consumed
                else
                    break
                end
            elseif wire_type == 1 then -- 64-bit
                current_offset = current_offset + 8
            elseif wire_type == 2 then -- length-delimited
                local len, consumed = decode_varint(buffer, current_offset)
                if len then
                    current_offset = current_offset + consumed + len
                else
                    break
                end
            elseif wire_type == 5 then -- 32-bit
                current_offset = current_offset + 4
            else
                break -- Unknown wire type
            end
        end
    end
    
    return current_offset - offset
end

local function parse_subscription(buffer, subtree, offset)
    local current_offset = offset
    local field_num, wire_type, consumed
    
    while current_offset < buffer:len() do
        field_num, wire_type, consumed = parse_key(buffer, current_offset)
        if not field_num then break end
        current_offset = current_offset + consumed
        
        if field_num == 1 and wire_type == 0 then -- vni (varint)
            local value, consumed = decode_varint(buffer, current_offset)
            if value then
                subtree:add(fields.subscription_vni, value)
                current_offset = current_offset + consumed
            end
        else
            -- Skip unknown field (same logic as in parse_hello)
            if wire_type == 0 then -- varint
                local _, consumed = decode_varint(buffer, current_offset)
                if consumed then
                    current_offset = current_offset + consumed
                else
                    break
                end
            elseif wire_type == 1 then -- 64-bit
                current_offset = current_offset + 8
            elseif wire_type == 2 then -- length-delimited
                local len, consumed = decode_varint(buffer, current_offset)
                if len then
                    current_offset = current_offset + consumed + len
                else
                    break
                end
            elseif wire_type == 5 then -- 32-bit
                current_offset = current_offset + 4
            else
                break -- Unknown wire type
            end
        end
    end
    
    return current_offset - offset
end

-- Simplified Update message parser (just identifies its presence)
local function parse_update(buffer, subtree, offset)
    subtree:add_proto_expert_info(expert.update_note)
    return buffer:len() - offset
end

-- Message Length handling for TCP reassembly
local function get_metalbond_message_length(buffer, offset)
    -- Check if we have enough bytes for the header (4 bytes)
    if buffer:len() - offset < 4 then
        -- Not enough data to determine length
        return 0
    end
    
    -- Read the version byte (must be 1)
    local version = buffer(offset, 1):uint()
    if version ~= 1 then
        -- Invalid version, can't determine message length
        return 0
    end
    
    -- Read the message length (2 bytes) from the header
    local msg_length = buffer(offset + 1, 2):uint()
    
    -- Total message size = header size (4 bytes) + protobuf payload length
    return 4 + msg_length
end

-- Dissector function
function metalbond_proto.dissector(buffer, pinfo, tree)
    local length = buffer:len()
    if length < 4 then 
        -- Not enough data for the header
        pinfo.desegment_offset = 0
        pinfo.desegment_len = DESEGMENT_ONE_MORE_SEGMENT
        return
    end

    pinfo.cols.protocol = metalbond_proto.name
    
    -- Parse the custom header
    local version = buffer(0, 1):uint()
    local msg_length = buffer(1, 2):uint()
    local msg_type = buffer(3, 1):uint()
    
    -- Check if we have the complete message
    if length < 4 + msg_length then
        -- Need more bytes
        pinfo.desegment_offset = 0
        pinfo.desegment_len = (4 + msg_length) - length
        return
    end
    
    local subtree = tree:add(metalbond_proto, buffer(), "MetalBond Protocol Data")
    
    -- Add header fields to the tree
    subtree:add(fields.header_version, version)
    subtree:add(fields.header_msg_length, msg_length)
    subtree:add(fields.header_msg_type, msg_type)
    
    -- Check header version
    if version ~= 1 then
        subtree:add_proto_expert_info(expert.malformed)
        pinfo.cols.info = "Invalid MetalBond version: " .. version
        return
    end
    
    -- Update the column info with message type
    local msg_type_str = ""
    if msg_type == 1 then msg_type_str = "HELLO"
    elseif msg_type == 2 then msg_type_str = "KEEPALIVE"
    elseif msg_type == 3 then msg_type_str = "SUBSCRIBE"
    elseif msg_type == 4 then msg_type_str = "UNSUBSCRIBE"
    elseif msg_type == 5 then msg_type_str = "UPDATE"
    else msg_type_str = "UNKNOWN(" .. msg_type .. ")"
    end
    pinfo.cols.info = "MetalBond " .. msg_type_str
    
    -- Create a subtree for the protobuf payload
    local protobuf_tree = subtree:add(metalbond_proto, buffer(4, msg_length), "MetalBond Payload")
    
    -- Extract protobuf payload for parsing
    local protobuf_buffer = buffer(4, msg_length):tvb()
    
    -- Basic protobuf parsing (simplified)
    local offset = 0
    
    -- Parse the protobuf message based on the message type
    if msg_type == 1 then -- HELLO
        offset = offset + parse_hello(protobuf_buffer, protobuf_tree, offset)
    elseif msg_type == 2 then -- KEEPALIVE
        -- Keepalive messages are typically empty
        protobuf_tree:add_proto_expert_info(expert.note)
    elseif msg_type == 3 then -- SUBSCRIBE
        offset = offset + parse_subscription(protobuf_buffer, protobuf_tree, offset)
    elseif msg_type == 4 then -- UNSUBSCRIBE
        -- Similar to subscribe
        offset = offset + parse_subscription(protobuf_buffer, protobuf_tree, offset)
    elseif msg_type == 5 then -- UPDATE
        offset = offset + parse_update(protobuf_buffer, protobuf_tree, offset)
    else
        protobuf_tree:add_proto_expert_info(expert.undecoded)
    end
    
    -- If there are additional messages in this TCP segment, try to dissect them too
    if buffer:len() > 4 + msg_length then
        local next_tvb = buffer:range(4 + msg_length):tvb()
        metalbond_proto.dissector(next_tvb, pinfo, tree)
    end
    
    return true
end

-- Register protocol preferences
metalbond_proto.prefs.tcp_port = Pref.uint("TCP Port", 4711, "MetalBond protocol TCP port")

-- Store default settings to handle preferences changes
local default_settings = {
    tcp_port = 4711
}

-- Registration
local tcp_port_table = DissectorTable.get("tcp.port")

-- Register the dissector for capture
tcp_port_table:add(default_settings.tcp_port, metalbond_proto)
tcp_port_table:add(53581, metalbond_proto)  -- Add the client port as well

-- If the user changes the port preference, handle it correctly
function metalbond_proto.prefs_changed()
    -- Unregister from old port numbers
    if default_settings.tcp_port ~= metalbond_proto.prefs.tcp_port then
        tcp_port_table:remove(default_settings.tcp_port, metalbond_proto)
        
        -- Register using new port number
        tcp_port_table:add(metalbond_proto.prefs.tcp_port, metalbond_proto)
        default_settings.tcp_port = metalbond_proto.prefs.tcp_port
    end
end 
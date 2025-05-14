# Metalbond Wireshark Dissector

This directory contains Wireshark dissector files for analyzing Metalbond protocol traffic.

## Files

- `metalbond.lua` - A basic Lua-based dissector that can provide simple protocol decoding
- `metalbond_advanced.lua` - A more advanced Lua dissector with better protobuf parsing and TCP reassembly
- `metalbond.proto` - A protobuf definition file that can be used with Wireshark's protobuf dissector

## Setup Instructions

### Installing the Lua Dissector

1. Copy either `metalbond.lua` (basic) or `metalbond_advanced.lua` (recommended) to your Wireshark plugins directory:
   - Windows: `%APPDATA%\Wireshark\plugins\` or `Wireshark\plugins\` in the installation directory
   - macOS: `/Users/[username]/.config/wireshark/plugins/` or `/Applications/Wireshark.app/Contents/PlugIns/wireshark/`
   - Linux: `~/.local/lib/wireshark/plugins/` or `/usr/lib/wireshark/plugins/`

2. Restart Wireshark

3. Wireshark will now automatically decode Metalbond protocol traffic on TCP port 4711

## Capturing Metalbond Traffic

To capture Metalbond traffic:

1. Start Wireshark and begin a capture on the appropriate interface
2. Apply a display filter: `tcp.port == 4711`
3. Start your Metalbond client and/or server
4. Observe the decoded traffic in Wireshark

## Dissector Comparison

1. **Basic Lua Dissector (`metalbond.lua`)**:
   - Simple implementation with basic message type detection
   - Limited protobuf parsing
   - Good for a quick overview of traffic

2. **Advanced Lua Dissector (`metalbond_advanced.lua`)**:
   - Improved protobuf parsing with field decoding
   - TCP stream reassembly support
   - Configurable through Wireshark preferences
   - Better heuristics for message identification

3. **Protobuf Dissector (with `metalbond.proto`)**:
   - Most complete and accurate decoding
   - Handles all message types and nested structures
   - Requires Wireshark's built-in protobuf support
   - Recommended for detailed protocol analysis

## Notes

- If your Metalbond server uses a different port than 4711, you can configure it in Wireshark preferences when using the Lua dissectors
- For the protobuf dissector, you'll need to update the port in the "Decode As" configuration

## Troubleshooting

If you're not seeing proper decoding:

1. Verify TCP port 4711 is being used (or configure the correct port)
2. Check Wireshark's "Enabled Protocols" list to ensure the Metalbond dissector is enabled
3. Try restarting Wireshark after adding the dissector files
4. If using the protobuf dissector, make sure you've selected the correct message type

## Message Types

The Metalbond protocol includes the following message types:

1. **Hello** - Initial connection setup with keepalive interval
2. **Subscription** - VNI (Virtual Network Identifier) subscription
3. **Update** - Network route updates with destination and next hop information 

#define RS "RootConstants(num32BitConstants=6, b0), SRV(t0), UAV(u0), UAV(u1)"
cbuffer Span : register(b0) {
    uint first;    // the first piece
    uint skew;     // where the copy starts in its piece 0
    uint size;     // the copy's buffer, in bytes (a multiple of 64 KiB)
    uint pieces;
    uint out_at;   // the first piece's dword in `differs`, in bytes
    uint compare;  // 0: the hashes are stored; else compared with those stored (2: and then stored)
};
ByteAddressBuffer now : register(t0);
RWByteAddressBuffer hashes : register(u0);
RWByteAddressBuffer differs : register(u1);
groupshared uint4 part[64];
[RootSignature(RS)]
[numthreads(64, 1, 1)]
void main(uint3 gid : SV_GroupID, uint3 tid : SV_GroupThreadID) {
    // One group to a piece; its 64 threads read 1 KiB at a time side by
    // side, each keeps its own hash, thread 0 folds the 64 in order.
    if (gid.x >= pieces) return;
    uint piece = first + gid.x;
    uint lo = piece * 65536u;
    lo = lo > skew ? lo - skew : 0u;
    uint hi = min((piece + 1u) * 65536u - skew, size);
    lo = lo & ~15u;
    hi = (hi + 15u) & ~15u;
    uint4 h = uint4(0x9E3779B9u, 0x85EBCA6Bu, 0xC2B2AE35u, 0x27D4EB2Fu);
    [loop]
    for (uint at = lo + tid.x * 16u; at < hi; at += 1024u) {
        h = (h ^ now.Load4(at)) * uint4(0x01000193u, 0x9E3779B1u, 0x85EBCA77u, 0xC2B2AE3Du);
        h = h.yzwx ^ (h >> 15);
    }
    part[tid.x] = h;
    GroupMemoryBarrierWithGroupSync();
    if (tid.x != 0u) return;
    uint4 g = uint4(0x9E3779B9u, 0x85EBCA6Bu, 0xC2B2AE35u, 0x27D4EB2Fu);
    [loop]
    for (uint t = 0u; t < 64u; t++) {
        g = (g ^ part[t]) * uint4(0x01000193u, 0x9E3779B1u, 0x85EBCA77u, 0xC2B2AE3Du);
        g = g.yzwx ^ (g >> 15);
    }
    if (compare != 0u) {
        differs.Store(out_at + gid.x * 4u, any(g != hashes.Load4(piece * 16u)) ? 1u : 0u);
        if (compare == 2u) hashes.Store4(piece * 16u, g);
    } else {
        hashes.Store4(piece * 16u, g);
    }
}

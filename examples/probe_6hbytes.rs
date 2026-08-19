use rusty_dds::Dds;
fn main(){let w=256u32;let n=(w*w) as usize;let mut s=Vec::with_capacity(n*4);
for i in 0..n{let x=(i as u32%w) as f32/w as f32;let y=(i as u32/w) as f32/w as f32;
s.extend_from_slice(&[x*8.0+(y*32.0).sin().abs(), y*4.0+(x*16.0).cos().abs(), (x*y*12.0).fract()*6.0, 1.0]);}
let d=Dds::encode_bc6h_uf16(&s,w,w).unwrap();
let mut h:u64=0xcbf29ce484222325; for b in &d.data {h^=*b as u64; h=h.wrapping_mul(0x100000001b3);}
println!("bc6h {:016x} {} bytes", h, d.data.len());}

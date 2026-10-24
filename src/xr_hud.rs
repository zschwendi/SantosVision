//! Qualified final-target HUD extraction. The flat game's output is never replaced.
//! Coverage comes from the original fragment's alpha at the same surviving raster
//! samples; it is not the final target alpha and never a difference of colors.
use std::sync::{atomic::{AtomicBool, Ordering}, LazyLock, Mutex};
use wgpu::*;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct HudFrame {
    pub abi_version:u32, pub byte_size:u32, pub lease:u64,
    pub provenance_epoch:u64, pub game_frame:u64, pub request_frame:u64, pub head_frame:u64,
    pub color_texture:u64, pub world_color_texture:u64, pub presentation_time:f64,
    pub width:u32, pub height:u32, pub source_eye:u32, pub flags:u32,
    pub angular_width_degrees:f32, pub distance_metres:f32, pub reserved:[u32;2],
}
const _: ()=assert!(std::mem::size_of::<HudFrame>()==104);
static READY:AtomicBool=AtomicBool::new(false);
static STATUS:Mutex<(u64,u64,Option<String>)>=Mutex::new((0,0,None));
#[no_mangle]
pub extern "C" fn sv_xr_set_hud_ready(value:u32)->i32 {
    if value>1{return -2;} READY.store(value==1,Ordering::Release);0
}
pub(crate) fn enabled()->bool { READY.load(Ordering::Acquire) && crate::xr_view::world_camera_enabled() }
pub fn status_json()->serde_json::Value { let s=STATUS.lock().unwrap_or_else(|p|p.into_inner());
    serde_json::json!({"abi_version":1,"ready":READY.load(Ordering::Acquire),"published":s.0,"rejected":s.1,"last_rejection":s.2,
      "coverage":"same raster, original shader alpha, over-composition; never color subtraction","angular_width_degrees":60,"distance_metres":2}) }
pub(crate) fn record_result(success:bool,reason:Option<&str>){let mut s=STATUS.lock().unwrap_or_else(|p|p.into_inner());if success{s.0+=1;s.2=None;}else{s.1+=1;s.2=reason.map(str::to_owned);}}

/// Immutable for the entire published world lease; the producer pool reuses an
/// image pair only when its Arc is owned by that pool alone.
pub(crate) struct HudImages { pub world:Texture, pub contribution:Texture }

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(crate) enum HudBlend { StraightOver, PremultipliedOver, Opaque, StencilOnly }
pub(crate) fn qualify_blend(blend:Option<BlendState>,mask:ColorWrites)->Option<HudBlend>{
    let rgb=mask & (ColorWrites::RED|ColorWrites::GREEN|ColorWrites::BLUE);
    if rgb.is_empty(){return Some(HudBlend::StencilOnly);}
    if rgb != ColorWrites::RED|ColorWrites::GREEN|ColorWrites::BLUE {return None;}
    let Some(blend)=blend else{return Some(HudBlend::Opaque);};
    let c=blend.color;
    if c.operation!=BlendOperation::Add{return None;}
    match (c.src_factor,c.dst_factor){
      (BlendFactor::SrcAlpha,BlendFactor::OneMinusSrcAlpha)=>Some(HudBlend::StraightOver),
      (BlendFactor::One,BlendFactor::OneMinusSrcAlpha)=>Some(HudBlend::PremultipliedOver),
      (BlendFactor::One,BlendFactor::Zero)=>Some(HudBlend::Opaque),_=>None,
    }
}
pub(crate) fn target(mode:HudBlend)->ColorTargetState{
    let color=BlendComponent{src_factor:if mode==HudBlend::StraightOver{BlendFactor::SrcAlpha}else{BlendFactor::One},dst_factor:BlendFactor::OneMinusSrcAlpha,operation:BlendOperation::Add};
    ColorTargetState{format:TextureFormat::Rgba16Float,blend:if mode==HudBlend::Opaque{None}else{Some(BlendState{color,alpha:BlendComponent{src_factor:BlendFactor::One,dst_factor:BlendFactor::OneMinusSrcAlpha,operation:BlendOperation::Add}})},write_mask:if mode==HudBlend::StencilOnly{ColorWrites::empty()}else{ColorWrites::ALL}}
}
/// Rewrites only the exact scalar color-return ABI used by the pinned original
/// shaders. main_inner, texture reads, clip/discard, stencil and output0 survive.
static FRAGMENT_RETURN:LazyLock<regex::Regex>=LazyLock::new(||regex::Regex::new(r"->\s*@location\(0u?\)\s*vec4<f32>\s*\{").unwrap());
pub(crate) fn fragment_abi_supported(source:&str)->bool{
    FRAGMENT_RETURN.find_iter(source).take(2).count()==1 && source.matches("return o0;").take(2).count()==1
        && !source.contains("@builtin(frag_depth)") && !source.contains("SVHudOutput")
}
pub(crate) fn augment_fragment(source:&str,mode:HudBlend)->Option<String>{
    if !fragment_abi_supported(source){return None;}
    let mut text=FRAGMENT_RETURN.replace(source,"-> SVHudOutput {").into_owned();
    let a=if mode==HudBlend::Opaque{"1.0f"}else{"sv_hud_source.w"};
    // The original final target is UNORM. Metal saturates source RGBA before
    // blending into UNORM; the floating auxiliary target must mirror that
    // conversion explicitly (including RGB), without touching output0.
    text=text.replace("return o0;",&format!("let sv_hud_source=clamp(o0, vec4<f32>(0.0f), vec4<f32>(1.0f)); return SVHudOutput(o0, vec4<f32>(sv_hud_source.xyz, {a}));"));
    text.push_str("\nstruct SVHudOutput { @location(0) original:vec4<f32>, @location(1) contribution:vec4<f32>, }\n");
    Some(text)
}

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(crate) enum ShaderKind { ScaleformVertex, ScaleformPixel, LensVertex, LensPixel, BlitVertex, BlitPixel, ClearVertex, ClearPixel, MapVertex, MapPixel, DepthToAlpha }
/// Cryptographic source pins, not labels supplied by a runtime index. Loading
/// another build cannot silently turn an arbitrary late gun/world pass into HUD.
pub(crate) fn certify_shader(hash:&str,source:&str)->Option<ShaderKind>{
    let (expected,kind)=PINS.iter().find(|(h,_,_)|*h==hash).map(|(_,s,k)|(*s,*k))?;
    if sha256(source.as_bytes())?==expected {Some(kind)}else{None}
}
fn sha256(bytes:&[u8])->Option<String>{
    #[cfg(any(target_os="macos",target_os="ios",target_os="visionos"))]
    {extern "C"{fn CC_SHA256(data:*const std::ffi::c_void,len:u32,out:*mut u8)->*mut u8;}
     let len=u32::try_from(bytes.len()).ok()?;let mut out=[0u8;32];unsafe{CC_SHA256(bytes.as_ptr().cast(),len,out.as_mut_ptr());}
     Some(out.iter().map(|v|format!("{v:02x}")).collect())}
    #[cfg(not(any(target_os="macos",target_os="ios",target_os="visionos")))]
    {let _=bytes;None}
}

const PINS:&[(&str,&str,ShaderKind)]=&[
    ("b8698e91b33c5e70","c5a59fe852281c2240093fc58f7aab3f4b6cf7c3abb7fc11a6c643b8cd7cb940",ShaderKind::LensVertex),
    ("4d38aec2eaae14db","280a48a80f00b7f0a92597b6011ee2c09cd8e914b5effa21658e9f67cfb363c6",ShaderKind::LensPixel),
    ("4e1d5d361e0ed0bc","6153fb6404c740ac049805cd05964a8e1ae36860f3f3f0c3e344e51335b8e538",ShaderKind::BlitVertex),
    ("01a05b914ee571f0","fe10028bfa9632ede16b0b02dd1864985d15f7ced89406419297ba0e7a15689f",ShaderKind::BlitPixel),
    ("d6ff3b94cf5ffede","f1d73a8b5d7cf984405e78bb744d2bd825df34e224607340f214ab060c538b33",ShaderKind::ClearVertex),
    ("90262b240184124a","76f23195a0b89485881617476f263bb40b978a50c99b09a10b069d0da88b0906",ShaderKind::ClearPixel),
    ("028921879df627f5","d8c49ebbf71f3e7e1d15064b6afeaf2484af704e37dd368e49d9bf93094193eb",ShaderKind::MapVertex),
    ("943b0ad9542193ff","9fefdff9f376aafa80ae232c3cff0971cbec8a121174aea6d26500c84d2f0200",ShaderKind::MapPixel),
    ("2d70b46951f3f092","29fe5f70356d2b82801b8697cdbdd853b10b84822725052b2f762e2dd9916a27",ShaderKind::MapVertex),
    ("826a317f7bd7b70a","0cb89e60b7054fa4069de37322de1c808fd162d06913e92657ccba3b3b1b97b6",ShaderKind::MapPixel),
    ("7f531ca66548aefe","0cae30e43edc8643a1033b91972ca4aff33926be8107a8c74ce8b62e0ad5f085",ShaderKind::DepthToAlpha),
    ("2b88ff319d737606","cdf86078c7a859adefdc32d7237b36735510a7bcdd5f37a27663356d68d07772",ShaderKind::ScaleformVertex),
    ("9e6fbdca24ad56a9","9212d73e785a59eded6d438476a5d45c7436fa3f0fc95034f9fab398ffe6a00f",ShaderKind::ScaleformVertex),
    ("f77dd9b8be5dea67","f74b591218a8c57a4eaaf8ae45baa39346a1dcc0deb63090f3931d2f9070608d",ShaderKind::ScaleformVertex),
    ("c4744678a7f23b0d","d24dae9f7ecc6e20d52bc6bf1c0c3641964bae23e3e4444bbb6e4e48d674eaf3",ShaderKind::ScaleformVertex),
    ("6b864ea6d4f670b2","ba71d062ff633ee1910bddb9715270095dcd4c2e041ad5a40e7d290f55a98269",ShaderKind::ScaleformVertex),
    ("5e339518ee770ae5","74903c3e896dbdd7fab786c402d21ebc219f4e33d2df2533d9f45c53b7f31e9c",ShaderKind::ScaleformVertex),
    ("0cac27a80be29c40","9a30cbde784dde60dd180c53967c14db35e181e8c8dfff342dc58da2e00d07d5",ShaderKind::ScaleformVertex),
    ("c87385d65b5321c4","da5eef91f7f439a0ad2bdf26cbb83c29be5382d8cfcef463e64bb4ba34fd866b",ShaderKind::ScaleformPixel),
    ("2c410e8d5d97ee93","648d8edd448a79fa508610dab1ea69cb3b6bb2dcaf36f0da1f4861005ce9c69e",ShaderKind::ScaleformPixel),
    ("52b17746b5ef04c4","bb66d16c1659ce721e0612bcd9314b263e94d85872274cf0996d7c6a158538dd",ShaderKind::ScaleformPixel),
    ("6eb493968c2b2b89","0ecb98504a3bffa70209ba2f018b0f5e154debdbd58b20484bd69846c0e8a4b1",ShaderKind::ScaleformPixel),
    ("fdd7018bbc8e5fc5","515f9adb22a903d46be046277b3b8d8fd5600806cc1cf718f1ff33b5fdfa515e",ShaderKind::ScaleformPixel),
    ("2ded65bff3c04164","cccb93a68645e1f5aa104de439ac4dc44b67a04fb1df81340b0f2d664d042d1d",ShaderKind::ScaleformPixel),
    ("910925b72468f04c","b338f3ec35b9a61c5c9364bcce311fd0b8332e092fcd41ec27ec9ac8a54e1e9a",ShaderKind::ScaleformPixel),
    ("a4f17f47278cc24c","0def64a7c6995e8add44ca403d6d97d21d6812ec2d492bb8f053b51e545b1636",ShaderKind::ScaleformPixel),
];

#[cfg(test)]
mod tests {
 use super::*;
 fn over()->BlendState {BlendState{color:BlendComponent{src_factor:BlendFactor::SrcAlpha,dst_factor:BlendFactor::OneMinusSrcAlpha,operation:BlendOperation::Add},alpha:BlendComponent{src_factor:BlendFactor::SrcAlpha,dst_factor:BlendFactor::OneMinusSrcAlpha,operation:BlendOperation::Add}}}
 #[test] fn rejects_partial_masks_and_destination_color_blends(){
  assert_eq!(qualify_blend(Some(over()),ColorWrites::ALL),Some(HudBlend::StraightOver));
  assert_eq!(qualify_blend(None,ColorWrites::ALL),Some(HudBlend::Opaque));
  assert_eq!(qualify_blend(None,ColorWrites::ALPHA),Some(HudBlend::StencilOnly));
  assert_eq!(qualify_blend(None,ColorWrites::RED),None);
  let mut b=over();b.color.src_factor=BlendFactor::Dst;assert_eq!(qualify_blend(Some(b),ColorWrites::ALL),None);
  b.color.src_factor=BlendFactor::One;assert_eq!(qualify_blend(Some(b),ColorWrites::ALL),Some(HudBlend::PremultipliedOver));
 }
 #[test] fn true_coverage_reconstructs_original_over_without_final_alpha(){
  for seed in 0..400 { let world=[0.12,0.37,0.81];let mut direct=world;let mut p=[0.;3];let mut coverage=0.;let mut wrong_alpha=0.;
   for n in 0..7 {let a=((seed*17+n*13)%101)as f64/100.;let rgb=[a*0.9,0.4,0.1];
    for c in 0..3{direct[c]=rgb[c]*a+direct[c]*(1.-a);p[c]=rgb[c]*a+p[c]*(1.-a);}
    coverage=a+coverage*(1.-a);wrong_alpha=a*a+wrong_alpha*(1.-a);
   }
   for c in 0..3{assert!((direct[c]-(p[c]+world[c]*(1.-coverage))).abs()<1e-12);}
   assert!(coverage>=wrong_alpha);
  }
 }
 #[test] fn augmentation_preserves_original_fragment_and_rejects_foreign_abi(){
  let src="var<private> o0:vec4<f32>; @fragment fn main(@builtin(position) p:vec4<f32>)-> @location(0u) vec4<f32> { if(p.x<1.){discard;} o0=vec4<f32>(0.5); return o0; }";
  let out=augment_fragment(src,HudBlend::StraightOver).unwrap();assert!(out.contains("if(p.x<1.){discard;}"));assert!(out.contains("SVHudOutput(o0, vec4<f32>(sv_hud_source.xyz, sv_hud_source.w))"));
  assert!(augment_fragment(&out,HudBlend::StraightOver).is_none());assert!(augment_fragment("@fragment fn main()-> @location(1) vec4<f32>{return vec4<f32>(1.);}",HudBlend::Opaque).is_none());
 }
 #[test] fn shader_certificate_rejects_arbitrary_index_label_or_changed_source(){
  assert_eq!(certify_shader("52b17746b5ef04c4","not the certified bytes"),None);
  assert_eq!(certify_shader("unknown","not the certified bytes"),None);
 }
 #[test] fn original_user_shader_pins_are_complete_and_fragment_abis_match(){
  let path=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("game-assets/playgta5-full-game-v1/playgta5-part-01-of-14.zip");
  if !path.exists(){eprintln!("original user shader ZIP unavailable; no asset qualification claimed");return;}
  use std::io::Read;let mut zip=zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
  for &(hash,_,kind) in PINS{let mut source=String::new();zip.by_name(&format!("mirror/playgta5.com/b/8b0b5899ed/shaders/{hash}.wgsl")).unwrap().read_to_string(&mut source).unwrap();
   assert_eq!(certify_shader(hash,&source),Some(kind));
   if matches!(kind,ShaderKind::ScaleformPixel|ShaderKind::BlitPixel){for mode in [HudBlend::StraightOver,HudBlend::Opaque]{assert!(augment_fragment(&source,mode).is_some(),"{hash}");}}
  }
 }
 #[test] fn hud_abi_is_additive_and_exact(){assert_eq!(std::mem::size_of::<HudFrame>(),104);assert_eq!(std::mem::align_of::<HudFrame>(),8);assert_eq!(std::mem::offset_of!(HudFrame,world_color_texture),56);assert_eq!(std::mem::offset_of!(HudFrame,presentation_time),64);assert_eq!(std::mem::offset_of!(HudFrame,reserved),96);}
}
#[cfg(all(test,target_os="macos"))]
#[path="xr_hud_metal_tests.rs"]
mod metal_tests;

//! D3D11 color output/input binding conflicts persistently null the SRV slot.
//! Apply at binding commands, before wgpu accumulates bind-group usages into a
//! render pass. Draw-only dummy substitution would resurrect a nulled binding
//! after the render target was unbound, which is not D3D11 behavior.
//!
//! https://learn.microsoft.com/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-omsetrendertargets
//! https://learn.microsoft.com/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-pssetshaderresources
//! Depth/stencil and their existing proxy/read-only handling are unchanged.
use super::*;

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
struct Subresources { mips:[u32;2], layers:[u32;2] }

impl Subresources {
    fn contains(self,mip:u32,layer:u32)->bool {
        self.mips[0]<=mip && mip<self.mips[1] && self.layers[0]<=layer && layer<self.layers[1]
    }
}

/// Normalize the immutable SRV descriptor exactly as srv_view does for its
/// declared view dimension. As in srv_view, cube-array `d` is used as a count
/// of array layers (not multiplied by six again).
/// A 3D mip is one D3D subresource regardless of a target's selected z slice.
fn sampled_subresources(desc:TexDesc,view:&ViewObj)->Option<Subresources> {
    if desc.mips==0 || desc.d==0 || view.is_buf {return None;}
    let dimension=match view.vdim {
        2=>TextureViewDimension::D1,
        4|6=>TextureViewDimension::D2,
        5|7=>TextureViewDimension::D2Array,
        8=>TextureViewDimension::D3,
        9=>TextureViewDimension::Cube,
        10=>TextureViewDimension::CubeArray,
        // wgpu has no 1D-array view. create_texture forwards dimension D1 and
        // an array count directly, which rejects a real D1 array; srv_view has
        // no D1-array mapping either. Do not invent a 2D reinterpretation here.
        _=>return None,
    };
    let first=view.a.min(desc.mips-1);
    let count=if view.b==0 || view.b==u32::MAX {desc.mips.saturating_sub(view.a)}else{view.b};
    let count=count.min(desc.mips.saturating_sub(view.a)).max(1);
    let layers=if matches!(dimension,TextureViewDimension::D2|TextureViewDimension::D2Array|TextureViewDimension::Cube|TextureViewDimension::CubeArray) {
        let first=view.c.min(desc.d-1);
        let count=match dimension {
            TextureViewDimension::D2=>1,
            TextureViewDimension::Cube=>6,
            _=>if view.d==0 {desc.d.saturating_sub(view.c)}else{view.d},
        };
        [first,first+count.min(desc.d.saturating_sub(view.c)).max(1)]
    }else{[0,1]};
    Some(Subresources{mips:[first,first+count],layers})
}

impl Gpu {
    /// Both currently resolved native resource parents must be the same;
    /// matching recycled guest IDs or differently allocated depth proxies are
    /// not sufficient. This only inspects the color outputs in s.rt.
    pub(super) fn color_safe_srv(&self,id:u32)->u32 {
        let Some(view)=self.view_obj(id).filter(|v|!v.is_buf) else{return id;};
        let Some(source)=self.tex(view.res) else{return id;};
        let Some(range)=sampled_subresources(source.desc,view) else{return id;};
        for target in &self.s.rt {
            let Some(destination)=self.tex(target.tex) else{continue;};
            if is_depth(destination.format) || source.tex!=destination.tex {continue;}
            let layer=if destination.desc.dim==TextureDimension::D3 {0}else{target.slice};
            if range.contains(target.mip,layer) {return 0;}
        }
        id
    }

    pub(super) fn unbind_color_target_srvs(&mut self) {
        for stage in 0..self.s.srv.len() {
            for slot in 0..self.s.srv[stage].len() {
                let id=self.s.srv[stage][slot];
                if id!=0 {
                    self.s.srv[stage][slot]=self.color_safe_srv(id);
                    #[cfg(all(target_os = "macos", feature = "native-draw-capture"))]
                    if let Some(capture)=&self.draw_capture {capture.color_srv_binding(self,26,stage,slot,id,self.s.srv[stage][slot]);}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn descriptor(vdim:u32,a:u32,b:u32,c:u32,d:u32)->ViewObj {
        ViewObj{res:1,is_buf:false,fmt:87,vdim,a,b,c,d,views:HashMap::new()}
    }
    fn desc()->TexDesc {TexDesc{w:64,h:64,d:12,mips:7,samples:1,dim:TextureDimension::D2}}
    #[test]
    fn only_intersecting_mips_and_array_layers_conflict() {
        let r=sampled_subresources(desc(),&descriptor(5,2,2,3,2)).unwrap();
        assert_eq!(r,Subresources{mips:[2,4],layers:[3,5]});
        for mip in 0..7 {for layer in 0..12 {assert_eq!(r.contains(mip,layer),[2,3].contains(&mip)&&[3,4].contains(&layer));}}
        assert_eq!(sampled_subresources(desc(),&descriptor(4,3,1,5,99)).unwrap(),Subresources{mips:[3,4],layers:[5,6]});
    }
    #[test]
    fn full_chain_and_cube_faces_match_actual_view_mapping() {
        for count in [0,u32::MAX] {
            assert_eq!(sampled_subresources(desc(),&descriptor(5,2,count,3,0)).unwrap(),Subresources{mips:[2,7],layers:[3,12]});
        }
        assert_eq!(sampled_subresources(desc(),&descriptor(9,0,1,6,0)).unwrap(),Subresources{mips:[0,1],layers:[6,12]});
        assert_eq!(sampled_subresources(desc(),&descriptor(10,1,2,0,12)).unwrap(),Subresources{mips:[1,3],layers:[0,12]});
    }
    #[test]
    fn three_dimensional_volume_is_one_subresource_per_mip() {
        let mut d=desc();d.dim=TextureDimension::D3;d.d=32;
        let r=sampled_subresources(d,&descriptor(8,2,1,8,4)).unwrap();
        assert_eq!(r,Subresources{mips:[2,3],layers:[0,1]});
        assert!(r.contains(2,0));assert!(!r.contains(1,0));assert!(!r.contains(3,0));
    }
    #[test]
    fn malformed_and_unsupported_descriptors_do_not_invent_aliases() {
        let mut d=desc();d.mips=0;assert!(sampled_subresources(d,&descriptor(4,0,1,0,0)).is_none());
        let mut d=desc();d.d=0;assert!(sampled_subresources(d,&descriptor(4,0,1,0,0)).is_none());
        let mut v=descriptor(4,0,1,0,0);v.is_buf=true;assert!(sampled_subresources(desc(),&v).is_none());
        assert!(sampled_subresources(desc(),&descriptor(1,0,1,0,0)).is_none());
        assert!(sampled_subresources(desc(),&descriptor(3,0,1,0,0)).is_none());
        assert_eq!(sampled_subresources(desc(),&descriptor(5,u32::MAX,u32::MAX,u32::MAX,u32::MAX)).unwrap(),
            Subresources{mips:[6,7],layers:[11,12]});
    }
}

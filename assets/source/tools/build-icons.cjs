// Rebuilds every platform icon export under assets/ from two masters:
// source/masters/vapor-foreground-1024.png (the sculpture, straight alpha)
// and marks/vapor-mark-orange.svg (the flat mark). The tile material and
// the enclosure curve come from surface.cjs. Requires Node and sharp; run
// from this directory with `npm install && npm run build`, which also runs
// build-marketing.cjs and make-previews.cjs.
//
// Not rebuilt here: the menu bar template and the browser favicons. Those
// are hand exports.
const fs=require('fs'),path=require('path'),sharp=require('sharp');
const root=path.resolve(__dirname,'..','..');
const MASTERS='source/masters';
const read=p=>fs.readFileSync(path.join(root,p));
function put(p,b){const f=path.join(root,p);fs.mkdirSync(path.dirname(f),{recursive:true});fs.writeFileSync(f,b);}
const json=(p,v)=>put(p,JSON.stringify(v,null,2)+'\n');
const svg=(body,w=1024,h=w)=>Buffer.from(`<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}">${body}</svg>`);
const {continuousPath,surface}=require('./surface.cjs');
async function resized(src,w,h=w){return sharp(src).resize(w,h,{fit:'fill',kernel:'lanczos3'}).png().toBuffer();}
function icns(reps){let chunks=[];for(const [name,n]of [['icp4',16],['icp5',32],['icp6',64],['ic07',128],['ic08',256],['ic09',512],['ic10',1024],['ic11',32],['ic12',64],['ic13',256],['ic14',512]]){let h=Buffer.alloc(8);h.write(name);h.writeUInt32BE(8+reps[n].length,4);chunks.push(h,reps[n]);}let b=Buffer.concat(chunks),h=Buffer.alloc(8);h.write('icns');h.writeUInt32BE(8+b.length,4);return Buffer.concat([h,b]);}
async function ico(src,sizes){
 const payloads=[];
 for(const n of sizes){
  const png=await resized(src,n);let bytes;
  if(n===256)bytes=png;
  else{
   const raw=await sharp(png).ensureAlpha().raw().toBuffer();const andStride=Math.ceil(n/32)*4;
   const h=Buffer.alloc(40);h.writeUInt32LE(40,0);h.writeInt32LE(n,4);h.writeInt32LE(n*2,8);h.writeUInt16LE(1,12);h.writeUInt16LE(32,14);h.writeUInt32LE(n*n*4+andStride*n,20);
   const xor=Buffer.alloc(n*n*4),and=Buffer.alloc(andStride*n);
   for(let y=0;y<n;y++)for(let x=0;x<n;x++){let i=(y*n+x)*4,j=((n-1-y)*n+x)*4;xor[j]=raw[i+2];xor[j+1]=raw[i+1];xor[j+2]=raw[i];xor[j+3]=raw[i+3];if(raw[i+3]===0)and[(n-1-y)*andStride+(x>>3)]|=1<<(7-x%8);}
   bytes=Buffer.concat([h,xor,and]);
  }payloads.push({n,bytes});
 }
 let h=Buffer.alloc(6);h.writeUInt16LE(1,2);h.writeUInt16LE(payloads.length,4);let off=6+16*payloads.length;const entries=[];
 for(const {n,bytes}of payloads){let e=Buffer.alloc(16);e[0]=n===256?0:n;e[1]=n===256?0:n;e.writeUInt16LE(1,4);e.writeUInt16LE(32,6);e.writeUInt32LE(bytes.length,8);e.writeUInt32LE(off,12);off+=bytes.length;entries.push(e);}return Buffer.concat([h,...entries,...payloads.map(x=>x.bytes)]);
}
(async()=>{
 const fg=read(`${MASTERS}/vapor-foreground-1024.png`);
 const alpha=await sharp(fg).extractChannel('alpha').toBuffer();
 const mono=await sharp({create:{width:1024,height:1024,channels:3,background:'#ffffff'}}).joinChannel(alpha).png().toBuffer();
 const flat=await sharp({create:{width:1024,height:1024,channels:3,background:'#FF4F00'}}).joinChannel(alpha).png().toBuffer();
 const bgLight=await sharp(svg('<defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#FFFFFF"/><stop offset="1" stop-color="#F1F0EE"/></linearGradient></defs><rect width="1024" height="1024" fill="url(#g)"/>')).removeAlpha().png().toBuffer();
 const bgDark=await sharp(svg('<defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#272A30"/><stop offset="1" stop-color="#15171B"/></linearGradient></defs><rect width="1024" height="1024" fill="url(#g)"/>')).removeAlpha().png().toBuffer();
 // The Icon Composer document carries its own copy of the foreground.
 put('macos/Vapor.icon/Assets/vapor-sculpted.png',fg);
 put(`${MASTERS}/vapor-mono-1024.png`,mono);put(`${MASTERS}/vapor-flat-orange-1024.png`,flat);
 put(`${MASTERS}/background-light-1024.png`,bgLight);put(`${MASTERS}/background-dark-1024.png`,bgDark);
 json(`${MASTERS}/background-colors.json`,{light:{solid_alternative:'#FAF9F7',gradient_top:'#FFFFFF',gradient_bottom:'#F1F0EE'},dark:{solid_alternative:'#17191D',gradient_top:'#272A30',gradient_bottom:'#15171B'},brand_orange:'#FF4F00',canvas:[1024,1024],background_is_full_bleed:true,foreground_is_straight_alpha:true});
 const compositions={};for(const [theme,bg]of [['light',bgLight],['dark',bgDark]]){
  const materialDefs=surface(theme).split('</defs>')[0]+'</defs>';
  const flatMaterial=await sharp(svg(materialDefs+'<rect width="1024" height="1024" fill="url(#porcelain)"/><rect width="1024" height="1024" fill="url(#upper)"/><rect width="1024" height="1024" fill="url(#lower)"/>')).removeAlpha().png().toBuffer();
  compositions[theme]=await sharp(flatMaterial).composite([{input:fg}]).png().toBuffer();put(`${MASTERS}/fullbleed-${theme}-1024.png`,compositions[theme]);
 }
 const maskPath=continuousPath(100,100,824);const maskSvg=svg(`<path fill="white" d="${maskPath}"/>`);put(`${MASTERS}/macos-legacy-mask.svg`,maskSvg);
 const maskRaw=await sharp(maskSvg).ensureAlpha().raw().toBuffer();
 const symmetric=Buffer.alloc(maskRaw.length);
 for(let y=0;y<1024;y++)for(let x=0;x<1024;x++){
  const samples=[[x,y],[1023-x,y],[x,1023-y],[1023-x,1023-y],[y,x],[1023-y,x],[y,1023-x],[1023-y,1023-x]];
  const a=Math.round(samples.reduce((sum,[xx,yy])=>sum+maskRaw[(yy*1024+xx)*4+3],0)/8),i=(y*1024+x)*4;
  symmetric[i]=symmetric[i+1]=symmetric[i+2]=255;symmetric[i+3]=a;
 }
 let mask=await sharp(symmetric,{raw:{width:1024,height:1024,channels:4}}).png().toBuffer();
 // render-system-mask.swift writes SwiftUI's own continuous mask here; it wins when present.
 const systemMask=path.join(root,`${MASTERS}/macos-system-mask-1024.png`);
 if(fs.existsSync(systemMask)){const meta=await sharp(systemMask).metadata();if(meta.width!==1024||meta.height!==1024||!meta.hasAlpha)throw new Error('System mask must be 1024px RGBA');mask=read(`${MASTERS}/macos-system-mask-1024.png`);}
 put(`${MASTERS}/macos-legacy-mask-1024.png`,mask);
 let masters={};
 for(const theme of ['light','dark']){
  const material=await sharp(svg(surface(theme))).png().toBuffer();
  const inset=await resized(fg,824);
  const placed=await sharp(material).composite([{input:inset,left:100,top:100}]).png().toBuffer();
  // Start opaque under the exact mask; this prevents double-antialiasing the edge.
  const opaque=await sharp(placed).flatten({background:theme==='light'?'#ECEBE9':'#242830'}).png().toBuffer();
  masters[theme]=await sharp(opaque).composite([{input:mask,blend:'dest-in'}]).png().toBuffer();
 }
 // The light legacy tile is the iconset's 1024 slot; only the dark one is kept as a master.
 put(`${MASTERS}/legacy-tile-dark-1024.png`,masters.dark);
 const reps={};for(const n of [16,32,64,128,256,512,1024])reps[n]=await resized(masters.light,n);
 for(const n of [16,32,128,256,512])for(const scale of [1,2]){const file=`icon_${n}x${n}${scale===2?'@2x':''}.png`;put('macos/Vapor.iconset/'+file,reps[n*scale]);}
 const windowsSizes=[16,20,24,30,32,36,40,48,60,64,72,80,96,128,256];
 put('windows/Vapor.ico',await ico(masters.light,windowsSizes));put('windows/Vapor-unplated.ico',await ico(fg,windowsSizes));
 for(const n of [...windowsSizes,512,1024]){put(`windows/png/vapor-${n}.png`,await resized(masters.light,n));put(`windows/unplated/vapor-${n}.png`,await resized(fg,n));}
 const targetSizes=[16,20,24,30,32,36,40,48,60,64,72,80,96,256];
 for(const n of targetSizes){const p=await resized(fg,n);for(const suffix of ['','_altform-unplated','_altform-lightunplated'])put(`windows/msix/Assets/AppList.targetsize-${n}${suffix}.png`,p);}
 const scales=[100,125,150,200,250,300,400];const tileSpecs={AppList:[44,44],SmallTile:[71,71],MedTile:[150,150],WideTile:[310,150],LargeTile:[310,310],StoreLogo:[50,50],SplashScreen:[620,300]};
 for(const [name,[w,h]]of Object.entries(tileSpecs))for(const sc of scales){
  const W=Math.round(w*sc/100),H=Math.round(h*sc/100);
  // Unplated art on a transparent square, or centered square within a wide asset.
  const side=Math.min(W,H),art=await resized(fg,side);
  const p=await sharp({create:{width:W,height:H,channels:4,background:'#00000000'}}).composite([{input:art,left:Math.floor((W-side)/2),top:Math.floor((H-side)/2)}]).png().toBuffer();
  put(`windows/msix/Assets/${name}.scale-${sc}.png`,p);if(sc===100)put(`windows/msix/Assets/${name}.png`,p);
  if(name!=='StoreLogo')put(`windows/msix/Assets/${name}.scale-${sc}_altform-colorful_theme-light.png`,p);
 }
 // Tray glyphs are the flat mark in black and white at common DPI sizes.
 const flatMark=read('marks/vapor-mark-orange.svg').toString();const d=flatMark.match(/ d="([^"]+)"/)[1];
 for(const [name,color]of [['black','#000'],['white','#fff']]){
  const s=svg(`<path fill="${color}" transform="translate(51 222) scale(3.565)" d="${d}"/>`);const p=await sharp(s).png().toBuffer();put(`windows/tray/Vapor-${name}.ico`,await ico(p,[16,20,24,32,40,48,64]));
 }
 const linuxSizes=[16,22,24,32,48,64,96,128,192,256,512,1024];
 for(const n of linuxSizes)put(`linux/hicolor/${n}x${n}/apps/vapor.png`,await resized(masters.light,n));
 const vector=svg(`${surface('light')}<path fill="#FF4F00" transform="translate(161 311) scale(2.72)" d="${d}"/>`);put('linux/hicolor/scalable/apps/vapor.svg',vector);
 put('linux/hicolor/symbolic/apps/vapor-symbolic.svg',svg(`<path fill="#2e3436" transform="translate(1 5.2) scale(.0853)" d="${d}"/>`,24));
 // Web touch icons use the full-bleed composition; the system applies its own corner mask.
 for(const [n,name]of [[180,'apple-touch-icon.png'],[192,'android-chrome-192x192.png'],[512,'android-chrome-512x512.png']])put('web/public/'+name,await resized(compositions.light,n));
 json(`${MASTERS}/geometry.json`,{canvas:[1024,1024],legacy_tile:{x:100,y:100,width:824,height:824,superellipse_exponent:5,optical_corner_reference:185,mask:fs.existsSync(systemMask)?'macos-system-mask-1024.png':'macos-legacy-mask.svg',outside_shadow:false,construction:'analytic Lame superellipse, 128 tangent-matched cubic segments; the native icon uses the system mask Icon Composer supplies'},native:{canvas:[1024,1024],background:'full bleed, no rounded mask',foreground:'single sculpted PNG layer; no tile or cast shadow; mono and flat alternatives included'}});
 console.log('Rebuilt the macOS fallback iconset, the Icon Composer foreground, the masters, Windows ICO/MSIX, Linux hicolor, and the web touch icons. sharp',sharp.versions.sharp);
})();
